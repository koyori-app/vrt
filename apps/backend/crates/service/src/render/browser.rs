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
///   よらず不可視にする（Playwright の `caret: 'hide'` と同じ機構）。
///   `caret-color` は継承プロパティなので、`*` セレクタが届かない
///   open shadow DOM の内側へも継承で波及する
/// - **有限アニメーション**（CSS animation / CSS transition / Web Animations API）:
///   `currentTime = endTime` へシークして pause。終端は仕様上ただ一つに
///   定まる状態（`fill: forwards` なら最終キーフレーム、無指定なら基底スタイル）
///   であり、壁時計に依存しない
/// - **無限アニメーション**: 終端が存在しないので `currentTime = 0` へ巻き戻して
///   pause。タイムライン座標 0 の絵はこれもただ一つに定まる。
///   「paused にするだけ」では**止まった位置**が撮影タイミング依存のままで、
///   flaky さは消えない——座標を明示的に固定するからこそ 2 回撮って同じ絵になる
/// - **今後始まる transition**: `transition-duration: 0s` で始まった瞬間に
///   終値へ飛ぶ（`transitionend` は発火するので、完了を待つ実装も壊さない）
///
/// シーク後に rAF を 2 回待って合成済みフレームへ反映させ、1 巡目の描画で
/// 新たに始まったアニメーションをもう一度同じ座標へ固定してから返る。
/// open shadow DOM と同一オリジン iframe は再帰的に辿る。closed shadow root、
/// クロスオリジン iframe、canvas / rAF 駆動の JS アニメーションには届かない
/// （モジュール末尾のテストと README の記載を参照）。
const FREEZE_SCRIPT: &str = r#"
(async () => {
  const CSS = [
    '*, *::before, *::after {',
    '  caret-color: transparent !important;',
    '  transition-duration: 0s !important;',
    '  transition-delay: 0s !important;',
    '}',
  ].join('\n');

  const freezeDoc = (doc) => {
    if (!doc) return;
    try {
      if (doc.head && !doc.getElementById('__vrt_freeze_style__')) {
        const style = doc.createElement('style');
        style.id = '__vrt_freeze_style__';
        style.textContent = CSS;
        doc.head.appendChild(style);
      }
    } catch (e) {}

    // document.getAnimations() は shadow tree の中を返さない実装があるため、
    // open shadow root は自前で辿って合流させる（Set で重複は消える）。
    const animations = new Set();
    try { for (const a of doc.getAnimations()) animations.add(a); } catch (e) {}
    const collectShadow = (root) => {
      let all;
      try { all = root.querySelectorAll('*'); } catch (e) { return; }
      for (const el of all) {
        const sr = el.shadowRoot;
        if (!sr) continue;
        for (const inner of sr.querySelectorAll('*')) {
          try { for (const a of inner.getAnimations()) animations.add(a); } catch (e) {}
        }
        collectShadow(sr);
      }
    };
    collectShadow(doc);

    for (const anim of animations) {
      try {
        const timing =
          anim.effect && anim.effect.getComputedTiming
            ? anim.effect.getComputedTiming()
            : null;
        const end = timing ? timing.endTime : NaN;
        // 有限は終端へ、無限は初期（タイムライン座標 0）へ。どちらも
        // 壁時計に依存しない一意な座標なので、2 回撮っても同じ絵になる。
        anim.currentTime = Number.isFinite(end) ? end : 0;
        anim.pause();
      } catch (e) {}
    }

    // 同一オリジンの iframe の中も同じ扱い。クロスオリジンは触れないので諦める。
    try {
      for (const frame of doc.querySelectorAll('iframe')) {
        try { freezeDoc(frame.contentDocument); } catch (e) {}
      }
    } catch (e) {}
  };

  const nextFrame = () =>
    new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));

  freezeDoc(document);
  await nextFrame();
  // 1 巡目の描画（スタイル適用・シーク）をきっかけに始まったアニメも同じ座標へ。
  freezeDoc(document);
  await nextFrame();
  return true;
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
        // 注入に失敗したまま撮ると flaky な絵が baseline に混ざるので、
        // 黙って続行せずこのストーリーの失敗として返す。
        if self.options.freeze_before_capture {
            page.evaluate(FREEZE_SCRIPT)
                .await
                .map_err(|source| RenderError::Cdp {
                    story_id: story_id.to_string(),
                    source,
                })?;
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
