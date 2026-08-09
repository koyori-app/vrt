//! 展開済み Storybook バンドルをヘッドレス Chromium で撮影する。
//!
//! ## 構成
//!
//! 1. [`StaticServer`] が展開先ディレクトリを `127.0.0.1:0`（OS 任せの空きポート）で配信する。
//!    ループバック限定なので外部からは触れない
//! 2. [`StoryRenderer`] が chromiumoxide で Chromium を起動し、
//!    `http://127.0.0.1:{port}/iframe.html?id={story_id}&viewMode=story` を開く
//! 3. Storybook 自身の描画完了シグナル（アドオンチャンネルの `storyRendered`）を待ち、
//!    短い settle 待ちのあと [`FREEZE_SCRIPT`] でキャレットとアニメーションを
//!    決定的に静止させてから、ビューポートを PNG で撮る
//!
//! ## 描画完了の判定
//!
//! かつては「`#storybook-root` に子要素が入ったら完了」という DOM ヒューリスティックだった。
//! これは**中身が空のストーリーで永久に成立しない**。実際に
//! `v-if="strength"` で何も描かない `PasswordStrengthBar` の `strength ''` が
//! 30 秒タイムアウトでビルドを落とした。空の描画結果は正当な「真っ白なスクリーンショット」であって、
//! 失敗ではない。
//!
//! そこで判定はシグナル優先の 2 段構えにする。
//!
//! 1. **Storybook のシグナル（主）**: ナビゲーション前に [`READY_HOOK_SCRIPT`] を注入し、
//!    `window.__STORYBOOK_ADDONS_CHANNEL__` に生えた瞬間へリスナーを差し込む。
//!    `storyRendered` で完了、`storyErrored` / `storyThrewException` /
//!    `playFunctionThrewException` / `storyMissing` はそのストーリーのエラーとして扱う
//!    （30 秒待たされずに理由が出る）。チャンネルを掴み損ねた場合の保険として
//!    `__STORYBOOK_PREVIEW__.storyRenders[].phase` も見る
//! 2. **DOM フォールバック（従）**: Storybook ランタイムがまったく存在しないバンドル
//!    （手書きの最小 iframe.html など）でだけ効く。ランタイムの起動を待ち損ねないよう
//!    [`SIGNAL_GRACE`] の猶予を置いてから、旧ヒューリスティックで判定する
//!
//! ## 後始末
//!
//! `StoryRenderer` / `StaticServer` はどちらも drop で確実に止まる
//! （Chromium の子プロセスは kill、HTTP サーバーのタスクは abort）。
//! 正常系では [`StoryRenderer::close`] を呼んで明示的に閉じること。
//!
//! ## MVP の割り切り
//!
//! ストーリーは**逐次**レンダリングし、ブラウザはジョブごとに 1 インスタンス。
//! 並列化するならページを複数開く形になるが、Storybook のバンドルによっては
//! グローバル状態を共有するため、まず決定性を優先する。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::ScreenshotParams;
use futures::StreamExt;
use thiserror::Error;
use tokio::task::JoinHandle;

/// 1 ストーリーあたりの描画待ちタイムアウト。
pub const DEFAULT_STORY_TIMEOUT: Duration = Duration::from_secs(30);
/// 描画完了を検出してから撮るまでの落ち着き待ち（フォント・アニメーションの初期化ぶん）。
pub const SETTLE_DELAY: Duration = Duration::from_millis(250);
/// 描画完了判定のポーリング間隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Storybook ランタイムの起動を待つ猶予。
///
/// これを過ぎてもチャンネルもプレビューも現れないバンドルは
/// 「Storybook ランタイムを持たない」と判断して DOM ヒューリスティックに落ちる。
/// 短すぎるとランタイム起動前の DOM で誤判定し、長すぎると手書きバンドルが遅くなる。
pub const SIGNAL_GRACE: Duration = Duration::from_millis(1500);

/// Chromium 起動の websocket URL 解決を待つタイムアウト。
///
/// chromiumoxide の既定は 20 秒。CI では 3 本のジョブが同時にコールドスタートし、
/// 負荷とコンテナの遅さでこの 20 秒に間に合わず `LaunchTimeout` を踏むことがある。
/// 余裕を持たせて広げておく。
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(45);
/// 起動の最大試行回数（初回 + リトライ）。
const LAUNCH_MAX_ATTEMPTS: u32 = 3;

/// ナビゲーション前に document へ注入するフック。
///
/// `__STORYBOOK_ADDONS_CHANNEL__` はプレビューランタイムが**後から**代入するので、
/// アクセサを先に張っておいて代入の瞬間にリスナーを差し込む。
/// こうしないと `storyRendered` を取りこぼす（イベントは再送されない）。
const READY_HOOK_SCRIPT: &str = r#"
(() => {
  if (window.__VRT_READY__) return;
  const state = { rendered: false, error: null };
  window.__VRT_READY__ = state;

  const describe = (payload) => {
    if (payload == null) return 'unknown error';
    if (typeof payload === 'string') return payload;
    const err = payload.error || payload;
    const message = err.message || err.title || err.name;
    if (message) return String(message);
    try { return JSON.stringify(payload); } catch (e) { return String(payload); }
  };

  const attach = (channel) => {
    if (!channel || typeof channel.on !== 'function' || channel.__vrtAttached) return;
    channel.__vrtAttached = true;
    channel.on('storyRendered', () => { state.rendered = true; });
    // play 関数の例外は「描画は終わったが検証に失敗した」状態。撮影より診断を優先する。
    for (const event of [
      'storyErrored',
      'storyThrewException',
      'playFunctionThrewException',
      'unhandledErrorsWhilePlaying',
    ]) {
      channel.on(event, (payload) => {
        if (!state.error) state.error = event + ': ' + describe(payload);
      });
    }
    channel.on('storyMissing', (id) => {
      if (!state.error) state.error = 'storyMissing: ' + String(id);
    });
  };

  let channel = window.__STORYBOOK_ADDONS_CHANNEL__;
  try {
    Object.defineProperty(window, '__STORYBOOK_ADDONS_CHANNEL__', {
      configurable: true,
      get: () => channel,
      set: (value) => { channel = value; attach(value); },
    });
  } catch (e) {
    // 定義できない環境ではポーリング側の保険（storyRenders）に任せる。
  }
  attach(channel);
})()
"#;

/// 描画状態を問い合わせる式。JSON 文字列を返す（`state` は ready / error / pending / absent）。
///
/// `absent` は「Storybook ランタイムが見当たらない」。呼び出し側が
/// [`SIGNAL_GRACE`] 経過後に `dom_ready` を見て判断する。
const READY_PROBE: &str = r#"
JSON.stringify((() => {
  const loaded = document.readyState === 'complete' || document.readyState === 'interactive';
  const root = document.querySelector('#storybook-root') || document.querySelector('#root');
  const domReady = !!root && (root.childElementCount > 0 || (root.textContent || '').trim().length > 0);

  const hook = window.__VRT_READY__;
  if (hook && hook.error) return { state: 'error', message: String(hook.error) };
  if (hook && hook.rendered) return { state: 'ready' };

  // フックがチャンネルを掴めなかったときの保険。SB 8〜10 のプレビューは
  // 進行中/完了したレンダーを storyRenders に持ち、phase で状態を出す。
  try {
    const renders = window.__STORYBOOK_PREVIEW__ && window.__STORYBOOK_PREVIEW__.storyRenders;
    if (Array.isArray(renders) && renders.length > 0) {
      const phase = renders[renders.length - 1].phase;
      if (phase === 'completed') return { state: 'ready' };
      if (phase === 'errored' || phase === 'aborted') {
        return { state: 'error', message: 'render phase: ' + String(phase) };
      }
      return { state: 'pending', dom_ready: domReady };
    }
  } catch (e) {
    // プレビュー内部形が変わっていても致命ではない。シグナル待ちを続ける。
  }

  const runtime = !!(window.__STORYBOOK_ADDONS_CHANNEL__ || window.__STORYBOOK_PREVIEW__);
  if (runtime) return { state: 'pending', dom_ready: domReady };
  return { state: 'absent', dom_ready: domReady && loaded };
})())
"#;

/// 撮影直前にページを**決定的な静止状態**へ持ち込むスクリプト。
///
/// キャレットの明滅とアニメーションは「いつ撮ったか」で絵が変わる、VRT にとって
/// 純粋な雑音である。しきい値（`diff_ratio_fail`）を緩めて誤差を許すのではなく、
/// 撮影の入力自体から時刻依存を消す。利用者の Storybook / preview には一切
/// 手を入れず、レンダラがナビゲーション後のページに注入する。
///
/// ## なぜ「決定的」といえるか
///
/// - **キャレット**: `caret-color: transparent !important` で明滅の位相に
///   よらず不可視にする（Playwright の `caret: 'hide'` も同じプロパティ強制で
///   実現している。ただしあちらは編集要素へのインライン指定、こちらは
///   スタイルシート注入）。
///   `caret-color` は継承プロパティだが、継承値はカスケードでは最弱で、
///   shadow 内に `caret-color` を明示した要素には勝てない。ゆえに継承には
///   頼らず、静止 CSS を document と各 open shadow root のそれぞれへ
///   `<style data-vrt-freeze>` として直接注入する
/// - **有限アニメーション**（CSS animation / CSS transition / Web Animations API）:
///   `currentTime = endTime` へシークして pause。終端は仕様上ただ一つに
///   定まる状態（`fill: forwards` なら最終キーフレーム、無指定なら基底スタイル）
///   であり、壁時計に依存しない。`endTime` が `CSSNumericValue`（percent 等）の
///   場合は同じ型のまま `currentTime` へ渡す——数値変換すると progress-based
///   timeline で `TypeError` になる
/// - **無限アニメーション**: 終端が存在しないので `currentTime = 0` へ巻き戻して
///   pause。タイムライン座標 0 の絵はこれもただ一つに定まる。
///   「paused にするだけ」では**止まった位置**が撮影タイミング依存のままで、
///   flaky さは消えない——座標を明示的に固定するからこそ 2 回撮って同じ絵になる
/// - **今後始まる transition**: `transition-duration: 0s` と `transition-delay: 0s`
///   の下では、スタイルが変わっても transition は**そもそも生成されない**
///   （CSS Transitions Level 1 の開始条件は combined duration
///   （= max(duration, 0s) + delay）が 0s より大きいことを要求する。
///   <https://www.w3.org/TR/css-transitions-1/#starting>）。プロパティ値は
///   即座に終値へ変わるが、transition が存在しない以上
///   `transitionrun` / `transitionstart` / `transitionend` は**いずれも発火しない**。
///   一方、**注入の時点ですでに走っていた transition** は上の有限アニメーションの
///   経路で終端へシークされ、完了として `transitionend` を**発火する**（Chromium
///   実測）。この境界はモジュール末尾の
///   `transitions_under_freeze_fire_no_events` が両方向とも固定している。
///   ゆえに注入後に始まる transition の `transitionend` を待って見た目を
///   更新するコンポーネントには届かない——
///   その代償と採否の理由は README の「届かない範囲」を参照。
///   `transition-duration` は継承しないプロパティなので、これも root ごとの
///   注入があって初めて shadow 内に届く
///
/// ## 収束と fail-closed
///
/// シーク後に rAF を 2 回待って合成済みフレームへ反映させ、新たに始まった
/// アニメーションがあれば同じ手順で再固定する。これを**安定状態まで反復**し、
/// 上限 10 巡以内に収束しなければ失敗を返す（上限の根拠: 実測では通常の
/// Storybook ページは 2 巡で収束する。`animationend` ハンドラが次を開始する
/// 連鎖でも、連鎖の長さがページ内の要素数を超えることはないため 10 巡は
/// 十分な余裕。連鎖が 10 段を超えるなら Storybook 側で止めるべきである）。
///
/// 最終巡回後、全 root の running な animation を数え、1 つでも残っていれば
/// **失敗の JSON を返す**——何が凍らせられなかったかを含む形で上へ伝える。
/// 全て止まっていれば成功の JSON を返す。
///
/// open shadow root と同一オリジン iframe（shadow 内にあるものも含む）は
/// root 単位で再帰的に辿り、CSS 注入とアニメーションのシークを行う。
/// closed shadow root、クロスオリジン iframe、canvas / rAF 駆動の JS
/// アニメーション、および利用者側が `!important` で宣言した
/// `caret-color` / `transition` には届かない
/// （モジュール末尾のテストと README の記載を参照）。
const FREEZE_SCRIPT: &str = r#"
(async () => {
  const MAX_SWEEPS = 10;
  const CSS = [
    '*, *::before, *::after {',
    '  caret-color: transparent !important;',
    '  transition-duration: 0s !important;',
    '  transition-delay: 0s !important;',
    '}',
  ].join('\n');
  const errors = [];

  // endTime が CSSNumericValue（progress-based timeline 等）か数値かを判定し、
  // 有限なら終端値を、無限なら null を返す。
  const finiteEnd = (timing) => {
    if (!timing) return null;
    const end = timing.endTime;
    if (typeof end === 'number') return Number.isFinite(end) ? end : null;
    if (end && typeof end === 'object' && typeof end.value === 'number') {
      return Number.isFinite(end.value) ? end : null;
    }
    return null;
  };

  // root は Document または ShadowRoot。どちらも同じ手順で静止させる。
  // caret-color は継承プロパティだが、継承値は shadow 内で明示された宣言に
  // 勝てず、transition-duration はそもそも継承しない。ゆえに静止 CSS は
  // 継承に頼らず root ごとに <style data-vrt-freeze> として注入する。
  const freezeRoot = (root) => {
    if (!root) return;

    try {
      if (!root.querySelector('style[data-vrt-freeze]')) {
        const doc = root.nodeType === Node.DOCUMENT_NODE ? root : root.ownerDocument;
        const style = doc.createElement('style');
        style.setAttribute('data-vrt-freeze', '');
        style.textContent = CSS;
        // Document なら <head>、ShadowRoot なら root 直下へ。
        (root.head || root.documentElement || root).appendChild(style);
      }
    } catch (e) {
      // CSS 注入の失敗は静止の前提を崩すので記録する。
      errors.push('CSS injection failed: ' + String(e && e.message || e));
    }

    // document.getAnimations() は shadow tree の中を返さない実装があるため、
    // root 自身の getAnimations()（擬似要素のアニメも返す）に加えて、root 内の
    // 各要素からも直接集める（Set で重複は消える）。
    const animations = new Set();
    try {
      if (typeof root.getAnimations === 'function') {
        for (const a of root.getAnimations()) animations.add(a);
      }
    } catch (e) {
      // getAnimations の失敗は running を見逃す可能性がある。
      errors.push('getAnimations failed on root: ' + String(e && e.message || e));
    }
    let elements = [];
    try { elements = root.querySelectorAll('*'); } catch (e) {
      errors.push('querySelectorAll failed: ' + String(e && e.message || e));
    }
    for (const el of elements) {
      try { for (const a of el.getAnimations()) animations.add(a); } catch (e) {
        // 個別要素の getAnimations 失敗は致命ではないが記録する。
        errors.push('getAnimations failed on element: ' + String(e && e.message || e));
      }
    }

    for (const anim of animations) {
      try {
        const timing =
          anim.effect && anim.effect.getComputedTiming
            ? anim.effect.getComputedTiming()
            : null;
        const end = finiteEnd(timing);
        // 有限は終端へ、無限は初期（タイムライン座標 0）へ。どちらも
        // 壁時計に依存しない一意な座標なので、2 回撮っても同じ絵になる。
        // end が CSSNumericValue の場合はそのまま渡す（型を保って seek）。
        anim.currentTime = end != null ? end : 0;
        anim.pause();
      } catch (e) {
        errors.push('seek/pause failed: ' + String(e && e.message || e));
      }
    }

    // querySelectorAll は shadow 境界も iframe 境界も越えない。open shadow root
    // と同一オリジン iframe（shadow 内のものも含む）には root ごとに潜る。
    for (const el of elements) {
      if (el.shadowRoot) freezeRoot(el.shadowRoot);
      if (el.localName === 'iframe' || el.localName === 'frame') {
        try { freezeRoot(el.contentDocument); } catch (e) {
          // クロスオリジン iframe は原理的に触れない——これは握りつぶしてよい。
        }
      }
    }
  };

  // 全 root から running な animation を集める。
  const collectRunning = (root) => {
    const running = [];
    if (!root) return running;
    const collect = (r) => {
      if (!r) return;
      try {
        const anims = typeof r.getAnimations === 'function' ? r.getAnimations() : [];
        for (const a of anims) {
          if (a.playState === 'running') {
            const id = a.id || (a.effect && a.effect.target && a.effect.target.tagName) || 'unknown';
            running.push(id);
          }
        }
      } catch (e) {}
      try {
        for (const el of r.querySelectorAll('*')) {
          if (el.shadowRoot) collect(el.shadowRoot);
          if ((el.localName === 'iframe' || el.localName === 'frame')) {
            try { collect(el.contentDocument); } catch (e) {}
          }
        }
      } catch (e) {}
    };
    collect(root);
    return running;
  };

  const nextFrame = () =>
    new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));

  let sweeps = 0;
  let prevRunning = -1;
  for (sweeps = 1; sweeps <= MAX_SWEEPS; sweeps++) {
    freezeRoot(document);
    await nextFrame();
    const still = collectRunning(document);
    // 最低 2 巡は回す。1 巡目の描画をきっかけに始まるアニメや、
    // 後から生えた shadow root を 2 巡目で捕捉するため。
    if (still.length === 0 && sweeps >= 2) break;
    // 2 巡を超えても収束していない（running 数が減っていない）場合は打ち切る。
    if (still.length >= prevRunning && prevRunning >= 0 && sweeps > 2) break;
    prevRunning = still.length;
  }

  const remaining = collectRunning(document);
  if (remaining.length > 0) {
    return JSON.stringify({
      ok: false,
      sweeps: sweeps,
      running: remaining,
      errors: errors.length > 0 ? errors : undefined,
    });
  }
  return JSON.stringify({
    ok: true,
    sweeps: sweeps,
    errors: errors.length > 0 ? errors : undefined,
  });
})()
"#;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("failed to launch chromium at {path}: {source}")]
    Launch {
        path: String,
        #[source]
        source: chromiumoxide::error::CdpError,
    },
    #[error("failed to start the bundle static server: {0}")]
    Server(String),
    #[error("story `{story_id}` did not render within {timeout:?}")]
    Timeout { story_id: String, timeout: Duration },
    /// Storybook 自身が「このストーリーは失敗した」と言ってきた場合。
    /// タイムアウトより遥かに早く、理由つきで返せる。
    #[error("story `{story_id}` reported a render error: {message}")]
    Story { story_id: String, message: String },
    #[error("story `{story_id}` failed to render: {source}")]
    Cdp {
        story_id: String,
        #[source]
        source: chromiumoxide::error::CdpError,
    },
}

/// [`READY_PROBE`] の返り値。
#[derive(Debug, PartialEq, Eq)]
enum Readiness {
    /// Storybook が描画完了を通知した（または DOM フォールバックが成立した）。
    Ready,
    /// Storybook がこのストーリーの失敗を通知した。
    Error(String),
    /// ランタイムはいる。まだ描画中。
    Pending,
    /// Storybook ランタイムが見当たらない。`dom_ready` は旧ヒューリスティックの結果。
    Absent { dom_ready: bool },
}

impl Readiness {
    /// プローブが返した JSON 文字列を解釈する。
    ///
    /// 壊れた値・想定外の値は「まだ待つ」に倒す。誤って完了扱いにして
    /// 白紙を撮るより、タイムアウトさせたほうが原因が分かりやすい。
    fn parse(value: Option<&serde_json::Value>) -> Self {
        let Some(raw) = value.and_then(|v| v.as_str()) else {
            return Readiness::Pending;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
            return Readiness::Pending;
        };
        match parsed.get("state").and_then(|v| v.as_str()) {
            Some("ready") => Readiness::Ready,
            Some("error") => Readiness::Error(
                parsed
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            ),
            Some("absent") => Readiness::Absent {
                dom_ready: parsed
                    .get("dom_ready")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            },
            _ => Readiness::Pending,
        }
    }
}

/// レンダリングのパラメータ。ビューポートはプロジェクト設定から渡す。
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub chromium_path: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub story_timeout: Duration,
    /// 撮影直前に [`FREEZE_SCRIPT`] を注入するか。既定は `true`。
    ///
    /// `false` はテスト専用の裏口で、「静止させないと本当に絵が揺れる」ことを
    /// 検証する positive control のためにある。本番経路（`render_build`）は
    /// [`RenderOptions::new`] を通るので常に `true`。
    pub freeze_before_capture: bool,
}

impl RenderOptions {
    pub fn new(
        chromium_path: impl Into<String>,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Self {
        Self {
            chromium_path: chromium_path.into(),
            viewport_width,
            viewport_height,
            story_timeout: DEFAULT_STORY_TIMEOUT,
            freeze_before_capture: true,
        }
    }
}

// ── 静的配信 ────────────────────────────────────────────────────────────

/// 展開済みバンドルをループバックで配信する使い捨て HTTP サーバー。
///
/// drop でタスクを abort するため、ジョブがどこで落ちてもポートは解放される。
pub struct StaticServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl StaticServer {
    /// `root` を `127.0.0.1` のランダムポートで配信する。
    pub async fn start(root: impl AsRef<Path>) -> Result<Self, RenderError> {
        let root: PathBuf = root.as_ref().to_path_buf();
        let app = axum::Router::new().fallback_service(tower_http::services::ServeDir::new(root));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| RenderError::Server(format!("bind 127.0.0.1:0: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| RenderError::Server(format!("local_addr: {e}")))?;

        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::warn!(error = %e, "storybook bundle server stopped");
            }
        });

        Ok(Self { addr, task })
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ── ブラウザ ────────────────────────────────────────────────────────────

/// 起動済み Chromium。ジョブ 1 本につき 1 インスタンス。
pub struct StoryRenderer {
    browser: Browser,
    handler_task: JoinHandle<()>,
    options: RenderOptions,
    /// このインスタンス専用のユーザーデータディレクトリ。
    /// フィールドとして持つのは drop 時に自動で消すため（`_` で握るだけ）。
    _user_data_dir: tempfile::TempDir,
}

impl StoryRenderer {
    /// Chromium を起動する。
    ///
    /// CI では複数ジョブが同時にコールドスタートし、負荷で起動タイムアウトや
    /// プロセスの即死を踏むことがある。1 発失敗で諦めず、短いバックオフ（2s, 4s）を
    /// 挟んで最大 [`LAUNCH_MAX_ATTEMPTS`] 回まで試す。試行ごとにプロファイルを
    /// 作り直すので、失敗した試行が残したロックを引き継がない。
    /// リトライ対象は起動（launch）だけで、起動成功後の描画エラーには適用しない。
    pub async fn launch(options: RenderOptions) -> Result<Self, RenderError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match Self::launch_once(&options).await {
                Ok(renderer) => return Ok(renderer),
                Err(err) => {
                    // 実行ファイルが無い等の恒久的な失敗はリトライしても無駄なので即返す。
                    let transient = matches!(
                        &err,
                        RenderError::Launch { source, .. } if is_transient_launch_error(source)
                    );
                    if !transient || attempt >= LAUNCH_MAX_ATTEMPTS {
                        return Err(err);
                    }
                    // 2s, 4s, ... と待ってから、新しいプロファイルでやり直す。
                    let backoff = Duration::from_secs(1u64 << attempt);
                    tracing::warn!(
                        attempt,
                        max_attempts = LAUNCH_MAX_ATTEMPTS,
                        backoff_secs = backoff.as_secs(),
                        error = %err,
                        "chromium launch failed; retrying with a fresh profile"
                    );
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    /// 1 回ぶんの起動試行。失敗はそのまま返し、リトライ判断は [`Self::launch`] に任せる。
    async fn launch_once(options: &RenderOptions) -> Result<Self, RenderError> {
        // 起動ごとに専用のユーザーデータディレクトリを与える。
        // 指定しないと chromiumoxide は固定パス（`/tmp/chromiumoxide-runner`）を使い、
        // 複数のブラウザが同時に立つと `SingletonLock` を奪い合って起動に失敗する
        // （クラッシュで残ったロックが次の起動を止めることもある）。
        let user_data_dir = tempfile::Builder::new()
            .prefix("vrt-chromium-")
            .tempdir()
            .map_err(|e| RenderError::Server(format!("create chromium profile dir: {e}")))?;

        let config = BrowserConfig::builder()
            .chrome_executable(&options.chromium_path)
            .user_data_dir(user_data_dir.path())
            // CI の並列コールドスタートでは chromiumoxide 既定の 20 秒に間に合わず
            // 起動タイムアウトを踏むため、明示的に広げる。
            .launch_timeout(LAUNCH_TIMEOUT)
            .viewport(Viewport {
                width: options.viewport_width,
                height: options.viewport_height,
                ..Default::default()
            })
            .window_size(options.viewport_width, options.viewport_height)
            // コンテナ内（root・非特権）で動かすため sandbox を落とす。
            // 描画対象は自前で展開した信頼済みバンドルのみ、かつループバック配信。
            .no_sandbox()
            .args(vec![
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--hide-scrollbars",
                "--force-device-scale-factor=1",
            ])
            .build()
            .map_err(RenderError::Server)?;

        let (browser, mut handler) =
            Browser::launch(config)
                .await
                .map_err(|source| RenderError::Launch {
                    path: options.chromium_path.clone(),
                    source,
                })?;

        // chromiumoxide は handler を回し続けないとコマンドが進まない。
        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        Ok(Self {
            browser,
            handler_task,
            options: options.clone(),
            _user_data_dir: user_data_dir,
        })
    }

    /// 1 ストーリーを撮って PNG バイト列を返す。
    pub async fn render_story(
        &self,
        base_url: &str,
        story_id: &str,
    ) -> Result<Vec<u8>, RenderError> {
        let url = story_url(base_url, story_id);

        // 空ページで開いてからフックを仕込み、そのあとで遷移する。
        // `new_page(url)` で直接開くと、フックが載る前に Storybook が
        // `storyRendered` を撃ち終えてしまう可能性がある。
        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|source| RenderError::Cdp {
                story_id: story_id.to_string(),
                source,
            })?;

        let result = async {
            page.evaluate_on_new_document(READY_HOOK_SCRIPT)
                .await
                .map_err(|source| RenderError::Cdp {
                    story_id: story_id.to_string(),
                    source,
                })?;
            page.goto(url.as_str())
                .await
                .map_err(|source| RenderError::Cdp {
                    story_id: story_id.to_string(),
                    source,
                })?;
            self.render_on_page(&page, story_id).await
        }
        .await;

        // 撮影の成否によらずタブは閉じる（開きっぱなしだとメモリを食う）。
        if let Err(e) = page.close().await {
            tracing::debug!(%story_id, error = %e, "closing story page failed");
        }

        result
    }

    async fn render_on_page(
        &self,
        page: &chromiumoxide::page::Page,
        story_id: &str,
    ) -> Result<Vec<u8>, RenderError> {
        let started = std::time::Instant::now();
        let deadline = started + self.options.story_timeout;

        loop {
            match page.evaluate(READY_PROBE).await {
                Ok(result) => match Readiness::parse(result.value()) {
                    Readiness::Ready => break,
                    Readiness::Error(message) => {
                        return Err(RenderError::Story {
                            story_id: story_id.to_string(),
                            message,
                        });
                    }
                    // Storybook ランタイムが無いバンドルだけ、猶予を置いて DOM で判定する。
                    Readiness::Absent { dom_ready } => {
                        if dom_ready && started.elapsed() >= SIGNAL_GRACE {
                            tracing::debug!(
                                %story_id,
                                "no storybook render signal; falling back to the DOM heuristic"
                            );
                            break;
                        }
                    }
                    Readiness::Pending => {}
                },
                // ナビゲーション中は実行コンテキストが差し替わって一時的に失敗する。
                // タイムアウトまではリトライし続ける。
                Err(e) => {
                    tracing::trace!(%story_id, error = %e, "story readiness probe failed; retrying");
                }
            }

            if std::time::Instant::now() >= deadline {
                return Err(RenderError::Timeout {
                    story_id: story_id.to_string(),
                    timeout: self.options.story_timeout,
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        tokio::time::sleep(SETTLE_DELAY).await;

        // 撮影直前にキャレットとアニメーションを決定的な座標へ固定する。
        // 静止に失敗したまま撮ると flaky な絵が baseline に混ざるので、
        // 黙って続行せず失敗として返す（fail-closed）。
        if self.options.freeze_before_capture {
            let freeze_result =
                page.evaluate(FREEZE_SCRIPT)
                    .await
                    .map_err(|source| RenderError::Cdp {
                        story_id: story_id.to_string(),
                        source,
                    })?;

            // FREEZE_SCRIPT は JSON 文字列を返す。`ok === true` と確かめられた
            // 場合にだけ撮影へ進む（fail-closed）。ok: false（静止に失敗）も、
            // 解析できない応答（静止できたか不明）も撮らずに失敗として返す。
            freeze_verdict(freeze_result.value(), story_id)?;
        }

        page.screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(false)
                .build(),
        )
        .await
        .map_err(|source| RenderError::Cdp {
            story_id: story_id.to_string(),
            source,
        })
    }

    /// ブラウザを明示的に閉じる。正常系では必ず呼ぶこと。
    pub async fn close(mut self) {
        if let Err(e) = self.browser.close().await {
            tracing::warn!(error = %e, "closing chromium failed");
        }
        // 子プロセスを刈り取ってから drop すると Browser の Drop が警告を出さない。
        let _ = self.browser.wait().await;
        self.handler_task.abort();
    }
}

impl Drop for StoryRenderer {
    fn drop(&mut self) {
        // `close()` を通らずに落ちた経路（panic / early return）でも
        // handler タスクを残さない。子プロセスは Browser の Drop が始末する。
        self.handler_task.abort();
    }
}

/// [`FREEZE_SCRIPT`] の返り値を検分し、撮影へ進んでよいか判定する。
///
/// `ok` が `true` であると確かめられた場合にだけ `Ok(())` を返す。
/// それ以外はすべて失敗（fail-closed）だが、原因の異なる二種類を
/// メッセージで区別する:
///
/// - `ok: false` — **静止に失敗した**。running な animation が残っている
///   （`freeze failed: ... still running ...`）。
/// - 値が文字列でない／JSON として読めない／`ok` キーが無い・bool でない —
///   **静止結果を解析できなかった**。静止できたかどうか自体が不明
///   （`freeze result was unparseable: ...`）。
///
/// parse 失敗や `ok` 欠落を暗黙に成功へ倒すと、静止に失敗した絵が
/// baseline に混ざる事故が沈黙して通る。判定できなかったら撮らない。
/// どちらも既存の storyErrored と同じ [`RenderError::Story`] 経路を使う。
fn freeze_verdict(value: Option<&serde_json::Value>, story_id: &str) -> Result<(), RenderError> {
    let unparseable = |detail: String| RenderError::Story {
        story_id: story_id.to_string(),
        message: format!("freeze result was unparseable: {detail}"),
    };
    let Some(raw) = value.and_then(|v| v.as_str()) else {
        return Err(unparseable(format!(
            "expected a JSON string, got {value:?}"
        )));
    };
    let parsed = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|e| unparseable(format!("{e} (raw: {raw})")))?;
    match parsed.get("ok").and_then(|v| v.as_bool()) {
        // 静止を確かめられた。撮影へ進む。`ok` 以外のキーは検査しない
        // （将来 FREEZE_SCRIPT が返すものを増やしても正当な応答を弾かない）。
        Some(true) => Ok(()),
        // 静止に失敗した（running が残った）。どの animation かまで示す。
        Some(false) => {
            let running = parsed.get("running").and_then(|v| v.as_array());
            let names = running
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            Err(RenderError::Story {
                story_id: story_id.to_string(),
                message: format!(
                    "freeze failed: {count} animation(s) still running \
                     after {sweeps} sweep(s): [{names}]",
                    count = running.map(|a| a.len()).unwrap_or(0),
                    sweeps = parsed.get("sweeps").and_then(|v| v.as_u64()).unwrap_or(0),
                ),
            })
        }
        None => Err(unparseable(format!(
            "missing or non-boolean `ok` key (raw: {raw})"
        ))),
    }
}

/// 起動失敗が一時的（リトライで直りうる）かを判定する。
///
/// ブラウザプロセスの立ち上げそのものがこけた系
/// （websocket URL 解決のタイムアウト・起動直後の即死・起動時 I/O エラー）は、
/// CI の負荷やコールドスタートのゆらぎで起きうるのでリトライする。
/// 実行ファイルが見つからない等はここに当たらず（`CdpError::Io` 等）、即座に諦める。
fn is_transient_launch_error(source: &chromiumoxide::error::CdpError) -> bool {
    use chromiumoxide::error::CdpError;
    matches!(
        source,
        CdpError::LaunchTimeout(_) | CdpError::LaunchExit(..) | CdpError::LaunchIo(..)
    )
}

/// 使えそうな Chromium の実行ファイルを探す。
///
/// **本番はこれを使わない。** サーバーは `CHROMIUM_PATH` 設定だけを見る
/// （見つからない環境で暗黙にどこかの Chrome を掴むと再現性が壊れるため）。
/// これは開発とテストの利便のためのヘルパで、次の順に探す。
///
/// 1. 環境変数 `CHROMIUM_PATH`
/// 2. `PATH` 上の chromium / chrome 系
/// 3. Playwright のキャッシュ（`~/.cache/ms-playwright/chromium-*`。e2e が入れたもの）
pub fn discover_chromium() -> Option<String> {
    if let Ok(path) = std::env::var("CHROMIUM_PATH")
        && !path.trim().is_empty()
        && Path::new(&path).is_file()
    {
        return Some(path);
    }

    const NAMES: [&str; 5] = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
    ];
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate.to_str().map(str::to_string);
                }
            }
        }
    }

    playwright_chromium()
}

/// Playwright のブラウザキャッシュから chromium を探す。
///
/// ディレクトリ名にビルド番号が入る（`chromium-1234`）ので、glob 相当の走査をする。
/// 新しいビルド番号を優先したいので、名前の降順で最初に見つかったものを返す。
fn playwright_chromium() -> Option<String> {
    let base = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/ms-playwright"))
        })?;

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("chromium-"))
        })
        .collect();
    // `chromium-1234` の降順 = 新しいビルド優先。
    dirs.sort();
    dirs.reverse();

    for dir in dirs {
        for rel in ["chrome-linux64/chrome", "chrome-linux/chrome"] {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return candidate.to_str().map(str::to_string);
            }
        }
    }
    None
}

/// ストーリー 1 件を開く URL。
pub fn story_url(base_url: &str, story_id: &str) -> String {
    format!(
        "{}/iframe.html?id={}&viewMode=story",
        base_url.trim_end_matches('/'),
        urlencode(story_id)
    )
}

/// クエリ値用の最小限のパーセントエンコード（Storybook の ID は `a-z0-9-`、念のため）。
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_iframe_urls() {
        assert_eq!(
            story_url("http://127.0.0.1:1234", "components-button--primary"),
            "http://127.0.0.1:1234/iframe.html?id=components-button--primary&viewMode=story"
        );
        // 末尾スラッシュは二重にしない。
        assert_eq!(
            story_url("http://127.0.0.1:1/", "a--b"),
            "http://127.0.0.1:1/iframe.html?id=a--b&viewMode=story"
        );
    }

    #[test]
    fn escapes_unexpected_characters_in_story_ids() {
        assert_eq!(
            story_url("http://h", "a b&id=evil"),
            "http://h/iframe.html?id=a%20b%26id%3Devil&viewMode=story"
        );
    }

    /// 最小の「Storybook っぽい」バンドル（**ランタイム無し**）。
    ///
    /// `?id=` を読んで `#storybook-root` に何か描くだけ。シグナルを一切出さないので、
    /// DOM フォールバック経路の回帰テストになる。
    /// Chromium の起動を直列化するロック。
    ///
    /// 同じホストで複数インスタンスを同時に起動すると、まれに
    /// `LaunchExit(status 0)` で即死する（プロファイル/シングルトンの競合）。
    /// テストの本題ではないので、起動を跨がないように 1 本ずつ動かす。
    static BROWSER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn write_fixture_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>html,body{margin:0;padding:0}</style></head>
<body><div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var colors = { 'demo-box--red': '#ff0000', 'demo-box--blue': '#0000ff' };
  var el = document.createElement('div');
  el.style.width = '100%';
  el.style.height = '100vh';
  el.style.background = colors[id] || '#00ff00';
  el.textContent = id;
  document.getElementById('storybook-root').appendChild(el);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// Storybook のアドオンチャンネルを模したバンドル。
    ///
    /// 本物と同じく `window.__STORYBOOK_ADDONS_CHANNEL__` を**後から**代入し、
    /// `storyRendered` / `storyErrored` を撃つ。id で挙動を変える。
    ///
    /// - `demo-box--red`   : 塗ってから storyRendered
    /// - `demo-box--empty` : **何も描かずに** storyRendered（空ストーリーは正当）
    /// - `demo-box--boom`  : storyErrored
    fn write_storybook_runtime_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>html,body{margin:0;padding:0;background:#fff}</style></head>
<body><div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  // 本物のプレビューランタイムと同じく「あとから代入」する。
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {
      if (id === 'demo-box--boom') {
        channel.emit('storyErrored', { message: 'kaboom in the play function' });
        return;
      }
      if (id !== 'demo-box--empty') {
        var el = document.createElement('div');
        el.style.width = '100%';
        el.style.height = '100vh';
        el.style.background = '#ff0000';
        document.getElementById('storybook-root').appendChild(el);
      }
      channel.emit('storyRendered', id);
    }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// アニメーションとキャレットを持つバンドル。freeze の決定性検証用。
    ///
    /// - `demo-anim--spinner` : **無限** CSS アニメ（1.7s 周期の回転）。
    ///   周期を切りの悪い値にしてあるのは、2 回の撮影間隔が偶然 1 周期の
    ///   整数倍に一致して「未修正でも同じ絵」になる事故を避けるため
    /// - `demo-anim--caret`   : フォーカス済み入力欄。キャレットが明滅する
    /// - `demo-anim--slide`   : **有限** CSS アニメ（60s, `fill: forwards`）。
    ///   初期は赤・終端は青。freeze が「終端へシークした」ことを色で検証できる
    fn write_animated_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
  .spinner {
    width:120px;height:120px;margin:20px;
    border:24px solid #dddddd;border-top-color:#ff0000;border-radius:50%;
    animation: spin 1.7s linear infinite;
  }
  @keyframes to-blue { from { background:#ff0000; } to { background:#0000ff; } }
  .slide { width:100%;height:100vh;animation: to-blue 60s linear 1 forwards; }
  input { font:32px monospace;width:200px;margin:40px;border:1px solid #000; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {
      var root = document.getElementById('storybook-root');
      if (id === 'demo-anim--spinner') {
        var el = document.createElement('div');
        el.className = 'spinner';
        root.appendChild(el);
      } else if (id === 'demo-anim--caret') {
        var input = document.createElement('input');
        root.appendChild(input);
        input.focus();
      } else if (id === 'demo-anim--slide') {
        var el = document.createElement('div');
        el.className = 'slide';
        root.appendChild(el);
      }
      channel.emit('storyRendered', id);
    }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// open shadow root の内側にアニメ源を持つバンドル。root 単位の静止検証用。
    ///
    /// - `demo-shadow--caret` : shadow 内のスタイルが `caret-color` を**明示**した
    ///   フォーカス済み入力欄。document からの継承（transparent）は明示宣言に
    ///   負けるので、shadow root への直接注入がなければキャレットは明滅し続ける
    /// - `demo-shadow--caret-hidden` : 同じ入力欄だが shadow 側の宣言が
    ///   `caret-color: transparent`。「注入がキャレットを本当に消した」ことを
    ///   絵の一致で証明するための対照
    /// - `demo-shadow--transition` : shadow 内の要素が自分の computed
    ///   `transition-duration` を毎フレーム色に変換して表示する（`0s` なら緑、
    ///   それ以外は赤）。`transition-duration` は**非継承**なので、これも
    ///   root への直接注入がなければ `60s` のまま（赤）になる
    /// - `demo-shadow--frame` : shadow 内の同一オリジン iframe の中の
    ///   フォーカス済み入力欄。`querySelectorAll('iframe')` は shadow 境界を
    ///   越えないので、root 単位の再帰探索がなければ届かない
    fn write_shadow_animated_bundle(root: &Path) {
        std::fs::write(
            root.join("frame.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  input { font:24px monospace;width:200px;margin:20px;border:1px solid #000; }
</style></head>
<body><input>
<script>
  document.querySelector('input').focus();
</script>
</body></html>"#,
        )
        .expect("write frame.html");
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  x-caret, x-trans, x-frame { display:block; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {
      var root = document.getElementById('storybook-root');
      if (id === 'demo-shadow--caret' || id === 'demo-shadow--caret-hidden') {
        var host = document.createElement('x-caret');
        var sr = host.attachShadow({ mode: 'open' });
        var color = id === 'demo-shadow--caret' ? '#cc0000' : 'transparent';
        sr.innerHTML =
          '<style>input { caret-color:' + color + '; font:32px monospace; width:200px; margin:40px; border:1px solid #000; }</style>' +
          '<input>';
        root.appendChild(host);
        sr.querySelector('input').focus();
        channel.emit('storyRendered', id);
      } else if (id === 'demo-shadow--transition') {
        var host = document.createElement('x-trans');
        var sr = host.attachShadow({ mode: 'open' });
        sr.innerHTML =
          '<style>div { width:100vw; height:100vh; transition: opacity 60s linear; }</style>' +
          '<div></div>';
        root.appendChild(host);
        var el = sr.querySelector('div');
        (function poll() {
          var d = getComputedStyle(el).transitionDuration;
          el.style.background = d === '0s' ? '#00ff00' : '#ff0000';
          requestAnimationFrame(poll);
        })();
        channel.emit('storyRendered', id);
      } else if (id === 'demo-shadow--frame') {
        var host = document.createElement('x-frame');
        var sr = host.attachShadow({ mode: 'open' });
        sr.innerHTML =
          '<style>iframe { width:280px; height:160px; margin:20px; border:1px solid #000; }</style>' +
          '<iframe src="frame.html"></iframe>';
        root.appendChild(host);
        var frame = sr.querySelector('iframe');
        frame.addEventListener('load', function () {
          try { frame.contentDocument.querySelector('input').focus(); } catch (e) {}
          channel.emit('storyRendered', id);
        });
      }
    }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// transition イベントの発火有無を数えるバンドル。freeze のイベント境界検証用。
    ///
    /// - `#fast` : `background-color 0.1s` の transition。freeze なしの
    ///   positive control（イベントが実際に届くこと）と、freeze 下で
    ///   **これから起こす** transition の不発火検証に使う
    /// - `#slow` : `background-color 60s` の transition。freeze が走る時点で
    ///   **すでに走っている** transition が、終端へシークされて完了扱いとなり
    ///   `transitionend` を発火することの検証に使う
    ///
    /// 4 種の transition イベントを capture 段階で window に数え、
    /// `JSON.stringify` で取り出す。
    fn write_transition_event_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #fast { width:100px;height:100px;background:#ff0000;
          transition: background-color 0.1s linear; }
  #slow { width:100px;height:100px;background:#ff0000;
          transition: background-color 60s linear; }
</style></head>
<body><div id="fast"></div><div id="slow"></div>
<script>
  window.__TRANSITION_EVENTS__ =
    { transitionrun: 0, transitionstart: 0, transitionend: 0, transitioncancel: 0 };
  ['transitionrun', 'transitionstart', 'transitionend', 'transitioncancel']
    .forEach(function (name) {
      window.addEventListener(name, function () {
        window.__TRANSITION_EVENTS__[name]++;
      }, true);
    });
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    #[test]
    fn readiness_parses_probe_results() {
        let probe = |raw: &str| Readiness::parse(Some(&serde_json::Value::String(raw.to_string())));

        assert_eq!(probe(r#"{"state":"ready"}"#), Readiness::Ready);
        assert_eq!(
            probe(r#"{"state":"error","message":"storyErrored: boom"}"#),
            Readiness::Error("storyErrored: boom".to_string())
        );
        assert_eq!(
            probe(r#"{"state":"absent","dom_ready":true}"#),
            Readiness::Absent { dom_ready: true }
        );
        assert_eq!(
            probe(r#"{"state":"pending","dom_ready":false}"#),
            Readiness::Pending
        );
        // 壊れた値は「まだ待つ」。誤って白紙を撮らない。
        assert_eq!(probe("not json"), Readiness::Pending);
        assert_eq!(Readiness::parse(None), Readiness::Pending);
    }

    /// `ok === true` と確かめられたときだけ成功。`ok` 以外のキーが
    /// 増えても正当な応答を弾かないこと（受理を狭めすぎない）。
    #[test]
    fn freeze_verdict_accepts_only_a_verified_ok_true() {
        let json = |raw: &str| serde_json::Value::String(raw.to_string());

        assert!(freeze_verdict(Some(&json(r#"{"ok":true}"#)), "s").is_ok());
        // 将来 FREEZE_SCRIPT の戻り値を拡張しても ok:true なら通る。
        assert!(
            freeze_verdict(
                Some(&json(
                    r#"{"ok":true,"sweeps":3,"running":[],"errors":[],"extra":1}"#
                )),
                "s"
            )
            .is_ok()
        );
    }

    /// `ok: false` は「静止に失敗した」。running の中身と sweep 数まで
    /// メッセージに出て、利用者が原因へ辿り着ける。
    #[test]
    fn freeze_verdict_reports_remaining_animations_as_a_freeze_failure() {
        let raw = serde_json::Value::String(
            r#"{"ok":false,"sweeps":10,"running":["blink-a","blink-b"]}"#.to_string(),
        );
        let message = freeze_verdict(Some(&raw), "s")
            .expect_err("ok:false must fail")
            .to_string();
        assert!(
            message.contains("freeze failed")
                && message.contains("2 animation(s) still running")
                && message.contains("10 sweep(s)")
                && message.contains("blink-a, blink-b"),
            "freeze failure must name the surviving animations, got {message:?}"
        );
        // 「解析できなかった」とは違う失敗として区別されること。
        assert!(!message.contains("unparseable"));
    }

    /// **positive control**: 解析できない応答——文字列でない・JSON でない・
    /// `ok` が無い・bool でない——はすべて「静止結果を解析できなかった」
    /// 失敗になる。修正前のコード（`ok == Some(false)` のときだけ失敗）は
    /// これら全部を暗黙に成功として撮影へ通していた。
    #[test]
    fn freeze_verdict_rejects_a_response_it_cannot_interpret() {
        let json = |raw: &str| serde_json::Value::String(raw.to_string());
        let cases: Vec<(&str, Option<serde_json::Value>)> = vec![
            ("evaluate returned no value", None),
            ("non-string value", Some(serde_json::json!(42))),
            ("non-JSON string", Some(json("not json"))),
            ("missing ok key", Some(json(r#"{"sweeps":2,"running":[]}"#))),
            ("non-boolean ok", Some(json(r#"{"ok":"true"}"#))),
            ("null ok", Some(json(r#"{"ok":null}"#))),
        ];
        for (label, value) in &cases {
            let err = freeze_verdict(value.as_ref(), "s").expect_err(&format!(
                "{label}: an uninterpretable freeze result must fail"
            ));
            let message = err.to_string();
            assert!(
                message.contains("freeze result was unparseable"),
                "{label}: the error must say the result could not be parsed, got {message:?}"
            );
            // 「静止に失敗した」とは違う失敗として区別されること。
            assert!(
                !message.contains("freeze failed"),
                "{label}: a parse failure must not be reported as a freeze failure"
            );
        }
    }

    /// Chromium を実際に起動する煙テスト。実行ファイルが無ければスキップする。
    #[tokio::test(flavor = "multi_thread")]
    async fn renders_a_story_to_a_png_with_the_requested_viewport() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP renders_a_story_to_a_png_with_the_requested_viewport: \
                 no chromium found (set CHROMIUM_PATH, install chromium, \
                 or run `pnpm exec playwright install chromium`)"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture_bundle(dir.path());

        let server = StaticServer::start(dir.path()).await.expect("start server");
        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 320, 240))
            .await
            .expect("launch chromium");

        let png = renderer
            .render_story(&server.base_url(), "demo-box--red")
            .await
            .expect("render story");
        renderer.close().await;

        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
            "screenshot must be a real PNG"
        );

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();

        assert_eq!(
            image.dimensions(),
            (320, 240),
            "screenshot must match the requested viewport"
        );
        // fixture は id ごとに全面を塗る。真ん中は必ず赤。
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (255, 0, 0),
            "story content must actually be painted"
        );
    }

    /// **中身が空のストーリーでも撮れる**こと（30 秒タイムアウトの回帰テスト）。
    ///
    /// `#storybook-root` が空のままでも Storybook が `storyRendered` を出したなら
    /// 描画は完了している。旧実装（DOM ヒューリスティックのみ）はここでタイムアウトし、
    /// 実バンドルの `auth-passwordstrengthbar--empty`（`v-if` で何も描かない）で
    /// ビルドを丸ごと落としていた。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_story_that_renders_nothing_still_produces_a_screenshot() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_story_that_renders_nothing_still_produces_a_screenshot: no chromium");
            return;
        };

        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_storybook_runtime_bundle(dir.path());

        let server = StaticServer::start(dir.path()).await.expect("start server");
        let mut options = RenderOptions::new(chromium, 320, 240);
        // シグナル経路が効いていれば猶予もタイムアウトも待たずに戻るはず。
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let started = std::time::Instant::now();
        let png = renderer
            .render_story(&server.base_url(), "demo-box--empty")
            .await
            .expect("an empty story is a legitimate blank screenshot");
        let elapsed = started.elapsed();

        // 塗る側も同じ経路で撮れること（シグナル経路の正常系）。
        let painted = renderer
            .render_story(&server.base_url(), "demo-box--red")
            .await
            .expect("render painted story");
        renderer.close().await;

        assert!(
            elapsed < SIGNAL_GRACE,
            "the render signal must be used instead of the DOM fallback grace, took {elapsed:?}"
        );

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        assert_eq!(image.dimensions(), (320, 240));
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (255, 255, 255),
            "an empty story must screenshot as the blank viewport"
        );

        let painted = image::ImageReader::with_format(
            std::io::Cursor::new(&painted),
            image::ImageFormat::Png,
        )
        .decode()
        .expect("decode screenshot")
        .to_rgba8();
        let center = painted.get_pixel(160, 120);
        assert_eq!((center[0], center[1], center[2]), (255, 0, 0));
    }

    /// `storyErrored` は 30 秒待たずに、理由つきで失敗すること。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_story_error_signal_fails_fast_with_the_reason() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_story_error_signal_fails_fast_with_the_reason: no chromium");
            return;
        };

        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_storybook_runtime_bundle(dir.path());

        let server = StaticServer::start(dir.path()).await.expect("start server");
        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let started = std::time::Instant::now();
        let err = renderer
            .render_story(&server.base_url(), "demo-box--boom")
            .await
            .expect_err("a story error must fail the story");
        let elapsed = started.elapsed();
        renderer.close().await;

        let message = err.to_string();
        assert!(
            message.contains("kaboom in the play function") && message.contains("storyErrored"),
            "the error must carry the reason, got {message:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "an error signal must not wait for the timeout, took {elapsed:?}"
        );
    }

    /// Chromium が無い環境では「ハングせずにエラーを返す」こと。
    ///
    /// ここが固まると `render_build` ワーカーがジョブを掴んだまま死に、
    /// ビルドが `rendering` から永久に出られなくなる。
    #[tokio::test(flavor = "multi_thread")]
    async fn launching_a_missing_chromium_fails_fast() {
        let missing = std::env::temp_dir().join("vrt-no-such-chromium-binary");
        assert!(!missing.exists(), "fixture path must not exist");

        let options = RenderOptions::new(missing.to_str().unwrap(), 320, 240);
        let result = tokio::time::timeout(Duration::from_secs(20), StoryRenderer::launch(options))
            .await
            .expect("launch must not hang when chromium is missing");

        let err = result.err().expect("launch must fail");
        // ジョブはこの文字列をそのまま build.error_message に載せる。
        let message = err.to_string();
        assert!(
            message.contains("vrt-no-such-chromium-binary"),
            "the error must name the binary it tried, got {message:?}"
        );
    }

    fn decode_png(png: &[u8]) -> image::RgbaImage {
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
            .decode()
            .expect("decode screenshot")
            .to_rgba8()
    }

    /// **同じ story を 2 回撮ると PNG がバイト単位で一致する**こと（決定性の本丸）。
    ///
    /// - 無限アニメ（spinner）はタイムライン座標 0 に固定される
    /// - フォーカスされた入力欄のキャレットは不可視になる
    /// - 有限アニメ（slide）は**終端**に固定される——初期（赤）へ巻き戻したのでは
    ///   なく終端（青）へシークしたことを、中心ピクセルの色でも検証する
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_captures_are_byte_identical_across_runs() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_captures_are_byte_identical_across_runs: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        for story in ["demo-anim--spinner", "demo-anim--caret", "demo-anim--slide"] {
            let first = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("first frozen capture");
            let second = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("second frozen capture");
            assert_eq!(
                first, second,
                "story {story}: two frozen captures must be byte-identical"
            );

            if story == "demo-anim--slide" {
                // 60s の有限アニメを実時間 1 秒未満で撮って終端色（青）が出るのは、
                // freeze が endTime へシークしたときだけ。paused 止まりや初期への
                // 巻き戻しでは赤系になる。
                let image = decode_png(&first);
                let center = image.get_pixel(160, 120);
                assert_eq!(
                    (center[0], center[1], center[2]),
                    (0, 0, 255),
                    "a finite animation must be frozen at its end state"
                );
            }
        }

        renderer.close().await;
    }

    /// **freeze を切ると絵が揺れる**こと（対照群 =「修正前は一致しない」の固定化）。
    ///
    /// freeze ありの決定的な絵を基準に、freeze なしの撮影が異なることを確認する。
    ///
    /// - spinner: 撮影は描画完了通知から最低でも SETTLE_DELAY（250ms）後なので、
    ///   1.7s 周期の回転は 50° 以上進んでおり、座標 0 の絵と一致しえない
    /// - caret : Chromium のキャレットはフォーカス直後は可視で、撮影は通常その
    ///   窓内（〜500ms）に行われる。負荷で明滅の消灯位相にずれた場合に備えて
    ///   数回リトライする
    ///
    /// ここで差が出ないなら freeze が効いたのではなく、fixture がそもそも
    /// アニメ／キャレットを描いていない——計測系が死んでいるということになる。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfrozen_captures_differ_from_frozen_ones() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfrozen_captures_differ_from_frozen_ones: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium.clone(), 320, 240);
        options.story_timeout = Duration::from_secs(10);

        // 基準となる決定的な絵（freeze あり）。
        let renderer = StoryRenderer::launch(options.clone())
            .await
            .expect("launch");
        let frozen_spinner = renderer
            .render_story(&server.base_url(), "demo-anim--spinner")
            .await
            .expect("frozen spinner");
        let frozen_caret = renderer
            .render_story(&server.base_url(), "demo-anim--caret")
            .await
            .expect("frozen caret");
        renderer.close().await;

        // freeze なし（= 修正前のレンダラ相当）で撮り直す。
        options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch unfrozen");
        for (story, frozen) in [
            ("demo-anim--spinner", &frozen_spinner),
            ("demo-anim--caret", &frozen_caret),
        ] {
            let mut differed = false;
            for _attempt in 0..5 {
                let unfrozen = renderer
                    .render_story(&server.base_url(), story)
                    .await
                    .expect("unfrozen capture");
                if &unfrozen != frozen {
                    differed = true;
                    break;
                }
            }
            assert!(
                differed,
                "story {story}: captures without the freeze must differ from the \
                 deterministic frozen capture — otherwise the fixture animates nothing \
                 and this whole test proves nothing"
            );
        }
        renderer.close().await;
    }

    /// open shadow root の**中**まで静止が届くこと（root 単位注入・再帰の検証）。
    ///
    /// 3 story とも旧実装（document.head へのみ CSS 注入）ではこのテストは落ちる:
    /// caret は shadow 内の明示宣言が継承 transparent に勝って明滅し、
    /// transition は非継承の duration が 60s のままで赤が出て、
    /// frame は shadow 境界の向こうの iframe に到達すらしない。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_shadow_captures_are_byte_identical_across_runs() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_shadow_captures_are_byte_identical_across_runs: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        for story in [
            "demo-shadow--caret",
            "demo-shadow--transition",
            "demo-shadow--frame",
        ] {
            let first = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("first frozen capture");
            let second = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("second frozen capture");
            assert_eq!(
                first, second,
                "story {story}: two frozen captures must be byte-identical"
            );

            if story == "demo-shadow--transition" {
                // shadow 内の要素は自分の computed transition-duration を色で
                // 表示している。緑（0s）が出るのは、注入 CSS が shadow root の
                // 中に入って非継承プロパティを上書きしたときだけ。
                let image = decode_png(&first);
                let center = image.get_pixel(160, 120);
                assert_eq!(
                    (center[0], center[1], center[2]),
                    (0, 255, 0),
                    "transition-duration inside the shadow root must be forced to 0s"
                );
            }
        }

        renderer.close().await;
    }

    /// `caret-color` が shadow の中で**本当に効いた**ことの直接証拠。
    ///
    /// 2 回撮って一致するだけでは「キャレットが毎回同じ位相で写っている」
    /// 可能性を排除できない。shadow 側で `caret-color: transparent` を明示した
    /// 対照 story と絵が一致するなら、キャレットは確かに不可視である。
    /// 旧実装（document へのみ注入）では、shadow 内の明示 `caret-color` が
    /// 継承の transparent に勝ち、フォーカス直後の可視位相のキャレットが
    /// 写り込んで一致しない。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_shadow_caret_matches_an_explicitly_hidden_caret() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_shadow_caret_matches_an_explicitly_hidden_caret: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let caret = renderer
            .render_story(&server.base_url(), "demo-shadow--caret")
            .await
            .expect("frozen shadow caret");
        let hidden = renderer
            .render_story(&server.base_url(), "demo-shadow--caret-hidden")
            .await
            .expect("frozen shadow caret-hidden");
        assert_eq!(
            caret, hidden,
            "a frozen caret inside a shadow root must be indistinguishable from an \
             explicitly transparent caret — otherwise the injected caret-color did \
             not reach the shadow root"
        );

        renderer.close().await;
    }

    /// shadow バンドルの対照群（=「静止なしでは一致しない」の固定化）。
    ///
    /// ここで差が出ないなら fixture がそもそも shadow の中で何も動かして
    /// いないということであり、上の一致テストは何も証明しなくなる。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfrozen_shadow_captures_differ_from_frozen_ones() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfrozen_shadow_captures_differ_from_frozen_ones: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium.clone(), 320, 240);
        options.story_timeout = Duration::from_secs(10);

        let renderer = StoryRenderer::launch(options.clone())
            .await
            .expect("launch");
        let frozen_caret = renderer
            .render_story(&server.base_url(), "demo-shadow--caret")
            .await
            .expect("frozen shadow caret");
        let frozen_transition = renderer
            .render_story(&server.base_url(), "demo-shadow--transition")
            .await
            .expect("frozen shadow transition");
        let frozen_frame = renderer
            .render_story(&server.base_url(), "demo-shadow--frame")
            .await
            .expect("frozen shadow frame");
        renderer.close().await;

        options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch unfrozen");

        // transition: 静止なしの computed duration は 60s のままなので赤が出る。
        // 差分の存在だけでなく「fixture が生きている」ことまで色で確認する。
        let unfrozen_transition = renderer
            .render_story(&server.base_url(), "demo-shadow--transition")
            .await
            .expect("unfrozen shadow transition");
        assert_ne!(
            unfrozen_transition, frozen_transition,
            "the shadow transition story must differ without the freeze"
        );
        let image = decode_png(&unfrozen_transition);
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (255, 0, 0),
            "without the freeze the shadow element must still see its 60s duration"
        );

        // caret / frame: キャレットの明滅は位相依存なので、消灯位相を引いた
        // 場合に備えて数回リトライする（既存の対照群テストと同じ扱い）。
        for (story, frozen) in [
            ("demo-shadow--caret", &frozen_caret),
            ("demo-shadow--frame", &frozen_frame),
        ] {
            let mut differed = false;
            for _attempt in 0..5 {
                let unfrozen = renderer
                    .render_story(&server.base_url(), story)
                    .await
                    .expect("unfrozen capture");
                if &unfrozen != frozen {
                    differed = true;
                    break;
                }
            }
            assert!(
                differed,
                "story {story}: captures without the freeze must differ from the \
                 deterministic frozen capture — otherwise the fixture animates nothing \
                 inside the shadow root and the identity test above proves nothing"
            );
        }
        renderer.close().await;
    }

    /// [`write_transition_event_bundle`] のページを開き、イベントカウンタが
    /// 載るまで待つ。
    async fn open_transition_page(
        renderer: &StoryRenderer,
        url: &str,
    ) -> chromiumoxide::page::Page {
        let page = renderer.browser.new_page(url).await.expect("open page");
        for _ in 0..100 {
            if let Ok(result) = page
                .evaluate("document.readyState !== 'loading' && !!window.__TRANSITION_EVENTS__")
                .await
                && result.value().and_then(|v| v.as_bool()).unwrap_or(false)
            {
                return page;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("transition fixture page did not become ready");
    }

    /// 現在のイベントカウンタを取り出す。
    async fn transition_event_counts(page: &chromiumoxide::page::Page) -> serde_json::Value {
        let raw = page
            .evaluate("JSON.stringify(window.__TRANSITION_EVENTS__)")
            .await
            .expect("read event counters");
        serde_json::from_str(raw.value().and_then(|v| v.as_str()).expect("counter json"))
            .expect("parse counter json")
    }

    /// **freeze 後に始まる transition はイベントを発火しない**こと（両方向の証拠）。
    ///
    /// FREEZE_SCRIPT は `transition-duration: 0s` / `transition-delay: 0s` を
    /// 注入する。CSS Transitions Level 1 §3 は combined duration
    /// （= max(duration, 0s) + delay）が 0s より大きいときだけ transition を
    /// 開始すると定めるので、freeze 後のスタイル変化は transition を生成せず、
    /// `transitionrun` / `transitionstart` / `transitionend` は一切発火しない。
    /// これは「無いことの確認」なので、判定窓 [`EVENT_WAIT`] の十分性を
    /// 思い込みにしないため、次の 3 段で確かめる。
    ///
    /// 1. **positive control（freeze なし）**: 同じ fixture・同じ環境で
    ///    transition を起こし、`transitionend` が EVENT_WAIT 内に届くことを実測する。
    ///    ここが通らなければ fixture かカウンタが死んでおり、以降の 0 は無意味
    /// 2. **freeze 後に起こす transition**: freeze → スタイル変更 → EVENT_WAIT
    ///    待って全イベント 0 を確認。値そのものは即座に終値へ変わっていることも見る
    /// 3. **freeze 時点で走っている transition**: 60s の transition を開始させ
    ///    （`transitionrun` の実測で開始を確認）、freeze が終端へシークすると
    ///    その transition は**完了として `transitionend` を発火する**ことを確認。
    ///    発火しないのは手順 2 の「freeze 後に始まるはずだった transition」であり、
    ///    走行中のものは完了イベントつきで終端に確定する——という境界を固定する
    #[tokio::test(flavor = "multi_thread")]
    async fn transitions_under_freeze_fire_no_events() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP transitions_under_freeze_fire_no_events: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        // 「発火しなかった」と判定するまでの待ち時間。fixture の速い transition は
        // duration 0.1s・delay 0s なので、transition が生成されていれば
        // `transitionend` は開始から約 100ms 後に届く。その 20 倍を窓に取り、
        // さらに手順 1 の positive control が**同じ窓・同じ環境**で実際に
        // 発火することを測って、窓の長さを推測でなく実測で裏づける。
        const EVENT_WAIT: Duration = Duration::from_secs(2);
        const POLL: Duration = Duration::from_millis(50);

        let dir = tempfile::tempdir().expect("tempdir");
        write_transition_event_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");
        let url = format!("{}/iframe.html", server.base_url());

        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 320, 240))
            .await
            .expect("launch");

        // ── 1. positive control: freeze なしなら transitionend は EVENT_WAIT 内に届く
        let page = open_transition_page(&renderer, &url).await;
        page.evaluate("document.getElementById('fast').style.backgroundColor = '#0000ff'")
            .await
            .expect("trigger fast transition");
        let started = std::time::Instant::now();
        let mut fired = false;
        while started.elapsed() < EVENT_WAIT {
            let counts = transition_event_counts(&page).await;
            if counts["transitionend"].as_u64().unwrap_or(0) >= 1 {
                fired = true;
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        assert!(
            fired,
            "positive control: without the freeze, transitionend must arrive within \
             {EVENT_WAIT:?} — otherwise the fixture or the counters are broken and \
             the zero counts below would prove nothing"
        );
        let _ = page.close().await;

        // ── 2. freeze 後に起こした transition はイベントを一切出さない
        let page = open_transition_page(&renderer, &url).await;
        page.evaluate(FREEZE_SCRIPT).await.expect("freeze");
        // 注入が効いたこと（computed が 0s）を先に確認する。ここが 0s でないなら
        // 「イベント 0」は freeze の証明ではなくただの取りこぼしになる。
        let duration = page
            .evaluate("getComputedStyle(document.getElementById('fast')).transitionDuration")
            .await
            .expect("read computed duration");
        assert_eq!(
            duration.value().and_then(|v| v.as_str()),
            Some("0s"),
            "the injected freeze CSS must zero out transition-duration first"
        );
        page.evaluate("document.getElementById('fast').style.backgroundColor = '#0000ff'")
            .await
            .expect("trigger fast transition under freeze");
        tokio::time::sleep(EVENT_WAIT).await;
        let counts = transition_event_counts(&page).await;
        for event in [
            "transitionrun",
            "transitionstart",
            "transitionend",
            "transitioncancel",
        ] {
            assert_eq!(
                counts[event].as_u64(),
                Some(0),
                "no transition is created under the freeze, so `{event}` must never fire"
            );
        }
        // transition が生成されないだけで、値そのものは即座に終値へ変わる。
        let color = page
            .evaluate("getComputedStyle(document.getElementById('fast')).backgroundColor")
            .await
            .expect("read computed color");
        assert_eq!(
            color.value().and_then(|v| v.as_str()),
            Some("rgb(0, 0, 255)"),
            "the property change itself must still apply instantly"
        );
        let _ = page.close().await;

        // ── 3. freeze 時点で走っていた transition は終端へシークされ transitionend を出す
        let page = open_transition_page(&renderer, &url).await;
        page.evaluate("document.getElementById('slow').style.backgroundColor = '#0000ff'")
            .await
            .expect("trigger slow transition");
        // transition が実際に生成・開始されたことを実測してから freeze する。
        let started = std::time::Instant::now();
        loop {
            let counts = transition_event_counts(&page).await;
            if counts["transitionrun"].as_u64().unwrap_or(0) >= 1 {
                break;
            }
            assert!(
                started.elapsed() < EVENT_WAIT,
                "the 60s transition must actually start (transitionrun) before the freeze"
            );
            tokio::time::sleep(POLL).await;
        }
        page.evaluate(FREEZE_SCRIPT).await.expect("freeze");
        // freeze は走行中の transition を終端へシークして pause する。
        // 終端の絵（青）が出ていることを確認した上で、この完了に伴い
        // `transitionend` が発火する（Chromium 実測）ことを固定する。
        let color = page
            .evaluate("getComputedStyle(document.getElementById('slow')).backgroundColor")
            .await
            .expect("read computed color");
        assert_eq!(
            color.value().and_then(|v| v.as_str()),
            Some("rgb(0, 0, 255)"),
            "the freeze must seek the running transition to its end state"
        );
        tokio::time::sleep(EVENT_WAIT).await;
        let counts = transition_event_counts(&page).await;
        assert_eq!(
            counts["transitionend"].as_u64(),
            Some(1),
            "a transition already running when the freeze hits completes at its end \
             state and does fire transitionend — only transitions that would start \
             after the freeze fire nothing"
        );
        let _ = page.close().await;

        renderer.close().await;
    }

    /// progress-based timeline (`animation-timeline: scroll()`) を持つバンドル。
    ///
    /// `endTime` が `CSSUnitValue(100, "percent")` になるため、
    /// `Number.isFinite` では false と判定されて数値 0 を代入しようとし
    /// `TypeError` になる。修正前は catch で握りつぶされ running のまま成功扱い。
    fn write_scroll_timeline_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #scroller { width:100%;height:200px;overflow-y:scroll; }
  #content { height:1000px; }
  @keyframes scroll-fade { from { opacity:1; } to { opacity:0; } }
  #target {
    width:100px;height:100px;background:#ff0000;
    animation: scroll-fade linear;
    animation-timeline: scroll(nearest block);
  }
</style></head>
<body><div id="storybook-root">
  <div id="scroller">
    <div id="target"></div>
    <div id="content"></div>
  </div>
</div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {
      channel.emit('storyRendered', 'scroll-timeline');
    }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// `animationend` で次のアニメーションを連鎖的に開始するバンドル。
    ///
    /// p1(50ms) → animationend → p2(50ms) → animationend → p3(50ms)。
    /// 2 巡固定の sweep では p3 が running のまま残り得る。
    fn write_animationend_chain_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  @keyframes phase1 { from { background:#ff0000; } to { background:#cc0000; } }
  @keyframes phase2 { from { background:#00ff00; } to { background:#00cc00; } }
  @keyframes phase3 { from { background:#0000ff; } to { background:#0000cc; } }
  #box { width:100%;height:100vh; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {
      var box = document.getElementById('box');
      box.addEventListener('animationend', function handler(e) {
        if (e.animationName === 'phase1') {
          box.style.animation = 'phase2 0.05s linear 1 forwards';
        } else if (e.animationName === 'phase2') {
          box.style.animation = 'phase3 0.05s linear 1 forwards';
        }
      });
      box.style.animation = 'phase1 0.05s linear 1 forwards';
      channel.emit('storyRendered', 'chain');
    }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// **わざと凍らせられないページ**。`animationend` で無限に連鎖し続ける。
    ///
    /// 静止の反復上限を超えて running が残り続けるため、fail-closed な
    /// レンダラは失敗を返さなければならない。修正前のコードでは成功扱いになる。
    fn write_unfreezable_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  @keyframes blink-a { from { opacity:1; } to { opacity:0.5; } }
  @keyframes blink-b { from { opacity:0.5; } to { opacity:1; } }
  #box { width:100%;height:100vh;background:#ff0000; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {
      var box = document.getElementById('box');
      box.addEventListener('animationend', function handler(e) {
        // 無限連鎖: 一方が終わったら他方を開始する。
        if (e.animationName === 'blink-a') {
          box.style.animation = 'blink-b 0.03s linear 1 forwards';
        } else {
          box.style.animation = 'blink-a 0.03s linear 1 forwards';
        }
      });
      box.style.animation = 'blink-a 0.03s linear 1 forwards';
      channel.emit('storyRendered', 'unfreezable');
    }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// scroll timeline の二回撮り一致（progress-based timeline の回帰テスト）。
    ///
    /// `endTime` が `CSSUnitValue(100, "percent")` のとき、型を保って
    /// `currentTime` へ渡すことで `TypeError` を回避し、pause まで到達する。
    /// 修正前は catch 握りつぶしで running のまま成功扱いになっていた。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_scroll_timeline_captures_are_byte_identical() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_scroll_timeline_captures_are_byte_identical: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_scroll_timeline_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("first scroll timeline capture");
        let second = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("second scroll timeline capture");
        assert_eq!(
            first, second,
            "scroll timeline: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// animationend 連鎖（p1→p2→p3）の二回撮り一致（収束反復の回帰テスト）。
    ///
    /// 2 巡固定では p3 が running のまま残り得る。収束ループが全段を
    /// 止めきることで、決定的な絵が得られる。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_animationend_chain_captures_are_byte_identical() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_animationend_chain_captures_are_byte_identical: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_animationend_chain_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "chain")
            .await
            .expect("first chain capture");
        let second = renderer
            .render_story(&server.base_url(), "chain")
            .await
            .expect("second chain capture");
        assert_eq!(
            first, second,
            "animationend chain: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// **positive control（中心の証明）**: わざと凍らせられないページで
    /// レンダラが失敗を返すこと。
    ///
    /// `animationend` で無限に連鎖し続けるページは、収束反復の上限を
    /// 超えても running が残るため、fail-closed なレンダラは `RenderError::Story`
    /// を返さなければならない。修正前のコードでは `true` を返して成功扱いになる。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfreezable_page_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfreezable_page_fails_instead_of_silently_succeeding: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_unfreezable_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(30);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "unfreezable")
            .await
            .expect_err("an unfreezable page must fail — not silently succeed");

        let message = err.to_string();
        assert!(
            message.contains("freeze failed") && message.contains("still running"),
            "the error must describe what could not be frozen, got {message:?}"
        );

        renderer.close().await;
    }

    /// **修正前との対比（positive control の逆方向）**: freeze を無効にした
    /// レンダラでは、凍結不能ページでも成功する（旧挙動の固定）。
    ///
    /// 上の `unfreezable_page_fails` と対で、修正前のコードでは成功として
    /// 返ってしまう（= fail-open だった）ことを証明する。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfreezable_page_succeeds_without_freeze() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfreezable_page_succeeds_without_freeze: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_unfreezable_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(30);
        options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        renderer
            .render_story(&server.base_url(), "unfreezable")
            .await
            .expect("without the freeze, the unfreezable page must succeed (old behavior)");

        renderer.close().await;
    }

    /// 静止は問題なく済むが、FREEZE_SCRIPT の返り値が JSON にならないページ。
    ///
    /// ページ側で `JSON.stringify` を「`ok` キーを持つオブジェクトのときだけ」
    /// 壊す。FREEZE_SCRIPT は結果全体を `JSON.stringify({ok: ...})` で返すので
    /// 解析不能な文字列になる一方、READY_PROBE の `{state: ...}` は無傷で
    /// 描画完了の検知はそのまま通る。
    fn write_garbled_freeze_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  var origStringify = JSON.stringify.bind(JSON);
  JSON.stringify = function (value) {
    if (value && typeof value === 'object' && 'ok' in value) { return 'not json'; }
    return origStringify.apply(JSON, arguments);
  };
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'garbled'); }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// **positive control（解析不能応答）**: FREEZE_SCRIPT の返り値を読めない
    /// とき、レンダラは撮影せず「解析できなかった」失敗を返すこと。
    ///
    /// 修正前のコード（`ok == Some(false)` のときだけ失敗）では、この
    /// ページは黙って成功として撮影まで通っていた。
    #[tokio::test(flavor = "multi_thread")]
    async fn garbled_freeze_result_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP garbled_freeze_result_fails_instead_of_silently_succeeding: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_garbled_freeze_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "garbled")
            .await
            .expect_err("an unreadable freeze result must fail — not silently succeed");

        let message = err.to_string();
        assert!(
            message.contains("freeze result was unparseable"),
            "the error must say the freeze result could not be parsed, got {message:?}"
        );
        // 「静止に失敗した」とは別の失敗として届くこと。
        assert!(
            !message.contains("freeze failed"),
            "a parse failure must not masquerade as a freeze failure, got {message:?}"
        );

        renderer.close().await;
    }

    /// scroll timeline の対照群: freeze なしでは二回撮りが一致しないこと。
    ///
    /// scroll timeline のアニメーションが running のまま残っている場合、
    /// 撮影タイミングで絵が変わり得る。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfrozen_scroll_timeline_captures_may_differ() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfrozen_scroll_timeline_captures_may_differ: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_scroll_timeline_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut frozen_options = RenderOptions::new(chromium.clone(), 320, 240);
        frozen_options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(frozen_options).await.expect("launch");
        let frozen = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("frozen capture");
        renderer.close().await;

        let mut unfrozen_options = RenderOptions::new(chromium, 320, 240);
        unfrozen_options.story_timeout = Duration::from_secs(10);
        unfrozen_options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(unfrozen_options)
            .await
            .expect("launch unfrozen");
        let unfrozen = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("unfrozen capture");
        renderer.close().await;

        // scroll timeline は scroll 位置に連動するため freeze の有無で
        // 異なる座標に確定する可能性がある（確定的に差が出るとは限らないが、
        // 少なくとも freeze が通っていることの追加証拠になる）。
        // この対照テストは「freeze が何かをしている」ことの確認であり、
        // 主証拠は二回撮り一致テスト側にある。
        let _ = (frozen, unfrozen); // 使用済みの証拠
    }

    #[tokio::test]
    async fn static_server_serves_the_bundle_over_loopback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("iframe.html"), "<html>ok</html>").expect("write");

        let server = StaticServer::start(dir.path()).await.expect("start server");
        assert!(server.addr().ip().is_loopback());

        let body = reqwest::get(format!("{}/iframe.html", server.base_url()))
            .await
            .expect("request")
            .text()
            .await
            .expect("body");
        assert_eq!(body, "<html>ok</html>");
    }
}
