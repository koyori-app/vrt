//! GitHub App 連携（PR コミットステータス・PR コメント）。
//!
//! - App 資格情報の組み立ては [`github_app`]。設定が無ければ `None` を返し、
//!   呼び出し側は「機能無効」として素通りする（起動は止めない）。
//! - Installation Access Token は Valkey にキャッシュする（[`installation_token`]）。
//!   GitHub のトークン有効期限は 1 時間なので、少し短い 50 分を TTL にする。
//! - コミットステータスの POST は [`post_commit_status`]。
//! - PR へのビルドリンクコメントは [`upsert_pr_comment`]（マーカー付きコメントを
//!   1 PR × 1 プロジェクトにつき 1 件だけ維持する）。
//!
//! ## forge-github との分担
//!
//! JWT 発行と installation token の取得は `forge-github` の [`GithubApp`] に任せる。
//! ベース URL は `GithubApp::with_api_base` で差し替えられるため、統合テストでは
//! wiremock を指す（`Settings::github_api_base_url`）。
//!
//! 一方 **コミットステータスの API は forge-github に無い**ため、
//! `POST /repos/{owner}/{repo}/statuses/{sha}` はこのモジュールで直接叩く
//! （auth-core 側は変更しない方針）。ベース URL は同じ設定値を使うので、
//! Enterprise / テストでも一貫して差し替わる。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use common::cache::redis::RedisConnection;
use common::settings::Settings;
use entity::builds;
use forge_github::{GithubApp, GithubAppCredentials};
use payload::github::GithubRepositoryResponse;

/// コミットステータスの `context`（GitHub の PR チェック一覧に出る名前）。
pub const STATUS_CONTEXT: &str = "vrt";

/// Installation Access Token のキャッシュ TTL（秒）。
/// GitHub 側の有効期限は 1 時間なので、期限ぎりぎりのトークンを掴まないよう短めにする。
pub const INSTALLATION_TOKEN_TTL_SECS: u64 = 50 * 60;

/// GitHub API 呼び出しの User-Agent（GitHub は UA 必須）。
const USER_AGENT: &str = "vrt";

/// repository 一覧で辿るページ数の上限（1 ページ 100 件）。
/// 暴走した場合の安全弁であって、通常は `total_count` に到達して先に抜ける。
/// ここに達したときは黙って切り捨てず、明示的なエラーにする。
const MAX_REPOSITORY_PAGES: u32 = 100;

/// GitHub API 呼び出しの失敗。リトライすべきかどうかで分ける。
#[derive(Debug, thiserror::Error)]
pub enum GithubApiError {
    /// ネットワーク断・5xx・レート制限など。ジョブのリトライで回復しうる。
    #[error("transient github api error: {0}")]
    Transient(anyhow::Error),
    /// 4xx（リポジトリが無い・権限が無い・SHA が不正など）。リトライしても直らない。
    #[error("permanent github api error: {0}")]
    Permanent(String),
}

/// 失敗レスポンスを Transient / Permanent に振り分ける。
///
/// レート制限は 429 のほか、**403 + `X-RateLimit-Remaining: 0`** でも返ってくる。
/// どちらも時間が経てば回復するので、[`GithubApiError`] の契約どおり Transient にする
/// （Permanent にすると 400 になり、呼び出し側が「入力を直せば通る」と誤解する）。
/// 回復時刻の手掛かりとして `Retry-After` / `X-RateLimit-Reset` をメッセージに残す。
fn classify_failure(
    context: &str,
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) -> GithubApiError {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("-")
    };

    let rate_limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || (status == reqwest::StatusCode::FORBIDDEN && header("x-ratelimit-remaining") == "0");
    if rate_limited {
        return GithubApiError::Transient(anyhow::anyhow!(
            "{context} rate limited: {status} retry_after={} rate_limit_reset={} {body}",
            header("retry-after"),
            header("x-ratelimit-reset"),
        ));
    }

    if status.is_client_error() {
        GithubApiError::Permanent(format!("{context} failed: {status} {body}"))
    } else {
        GithubApiError::Transient(anyhow::anyhow!("{context} failed: {status} {body}"))
    }
}

/// コミットステータスの state。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitState {
    Pending,
    Success,
    Failure,
    Error,
}

impl CommitState {
    pub fn as_str(self) -> &'static str {
        match self {
            CommitState::Pending => "pending",
            CommitState::Success => "success",
            CommitState::Failure => "failure",
            CommitState::Error => "error",
        }
    }
}

impl std::fmt::Display for CommitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 設定から `forge-github` の App クライアントを組み立てる。
///
/// `GITHUB_APP_ID` と `GITHUB_APP_PRIVATE_KEY_PEM` の両方が無ければ `None`
/// （＝ GitHub 連携は無効）。秘密鍵の妥当性はここでは検証しない
/// （JWT 発行時にエラーになる）。
pub fn github_app(settings: &Settings, http: &reqwest::Client) -> Option<GithubApp> {
    let app_id = settings.github_app_id?;
    let pem = settings.github_app_private_key_pem.as_ref()?;
    Some(
        GithubApp::new(
            http.clone(),
            GithubAppCredentials::new(app_id.to_string(), pem.clone()),
        )
        .with_api_base(settings.github_api_base_url())
        .with_user_agent(USER_AGENT),
    )
}

/// Installation Access Token のキャッシュキー。
fn token_cache_key(installation_id: i64) -> String {
    format!("github:installation_token:{installation_id}")
}

/// installation_id ごとのトークン取得ロック（プロセス内 single-flight）。
///
/// 同一 installation に対するトークン取得を直列化し、キャッシュミス時の GitHub
/// への二重取得を防ぐ。std の `Mutex` は `Arc<tokio::sync::Mutex>` の取り出しだけに
/// 使い、`await` をまたいで保持しない。
///
/// エントリは一度作られると解放しないが、キーは installation_id（高々テナント数
/// オーダー）なので無限成長にはならず、掃除は不要。
static TOKEN_FETCH_LOCKS: OnceLock<Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

/// 指定 installation の取得ロックを取り出す（無ければ作る）。
fn token_fetch_lock(installation_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    let map = TOKEN_FETCH_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("token fetch lock map poisoned");
    guard.entry(installation_id).or_default().clone()
}

/// Installation Access Token を取得する（Valkey キャッシュ経由）。
///
/// まずロック無しで楽観的にキャッシュを読み、ヒットすればそのまま返す。ミス時は
/// installation_id 単位のプロセス内ロックを取り、キャッシュを再確認（double-checked）
/// してから GitHub に取りに行き、結果をキャッシュして返す。これによりビルド 1 本で
/// 複数のコミットステータス送信がほぼ同時に走っても、トークン取得は 1 回に収束する。
///
/// single-flight は**プロセス内のみ**で、複数プロセス間の重複取得は許容する
/// （GitHub API 上は問題ない）。キャッシュの読み書きに失敗しても致命的ではないため
/// 警告ログだけ出して素通りする（毎回取りに行くだけ）。
pub async fn installation_token(
    redis: &RedisConnection,
    app: &GithubApp,
    installation_id: i64,
) -> Result<String, GithubApiError> {
    let key = token_cache_key(installation_id);

    // 楽観的な先読み。ヒット時はロック不要で速い。
    match cache_get(redis, &key).await {
        Ok(Some(token)) => return Ok(token),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, installation_id, "github token cache read failed"),
    }

    // ミス時は per-installation ロックで直列化してから再確認する。
    let lock = token_fetch_lock(installation_id);
    let _guard = lock.lock().await;

    // double-checked: ロック待ちの間に別タスクが取得・キャッシュ済みかもしれない。
    match cache_get(redis, &key).await {
        Ok(Some(token)) => return Ok(token),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, installation_id, "github token cache read failed"),
    }

    // 4xx / 5xx の区別が付かない（forge-github は anyhow の文字列にまとめる）ため、
    // トークン取得の失敗は一律 transient 扱いにしてジョブのリトライに委ねる。
    // 設定ミス（App ID / 秘密鍵の誤り）ならリトライ上限まで失敗して諦める。
    let token = app
        .installation_access_token(installation_id)
        .await
        .map_err(GithubApiError::Transient)?;

    if let Err(e) = cache_set(redis, &key, &token.token, INSTALLATION_TOKEN_TTL_SECS).await {
        tracing::warn!(error = %e, installation_id, "github token cache write failed");
    }

    Ok(token.token)
}

async fn cache_get(redis: &RedisConnection, key: &str) -> Result<Option<String>, anyhow::Error> {
    let mut conn = redis
        .conn
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("redis acquire failed: {e}"))?;
    redis::cmd("GET")
        .arg(key)
        .query_async(&mut conn)
        .await
        .map_err(|e| anyhow::anyhow!("redis GET failed: {e}"))
}

async fn cache_set(
    redis: &RedisConnection,
    key: &str,
    value: &str,
    ttl_secs: u64,
) -> Result<(), anyhow::Error> {
    let mut conn = redis
        .conn
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("redis acquire failed: {e}"))?;
    redis::cmd("SET")
        .arg(key)
        .arg(value)
        .arg("EX")
        .arg(ttl_secs)
        .exec_async(&mut conn)
        .await
        .map_err(|e| anyhow::anyhow!("redis SET failed: {e}"))
}

// ── GitHub App インストール導線の one-time state ────────────────────────────
//
// GitHub の setup URL には任意の `installation_id` と `state` を付けて他人に踏ませられる。
// state をサーバ側で発行・保存し、claim 時に消費・照合することで
// 「攻撃者の installation を、罠 URL を踏んだ admin のテナントに紐付ける」経路を塞ぐ。

/// setup state の有効期限（秒）。GitHub のインストール画面を操作する時間を見て 15 分。
pub const SETUP_STATE_TTL_SECS: u64 = 15 * 60;

const SETUP_STATE_PREFIX: &str = "github:setup_state:";

/// setup state に結び付けた発行元。claim 時にこの 2 つが一致しなければ拒否する。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SetupState {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
}

/// インストール開始時に one-time state を発行して Valkey に保存する。
pub async fn issue_setup_state(
    redis: &RedisConnection,
    user_id: Uuid,
    tenant_id: Uuid,
) -> Result<String, AppError> {
    let state = auth_core::pkce::generate_state();
    let payload = serde_json::to_string(&SetupState { user_id, tenant_id })
        .map_err(|e| AppError::Internal(anyhow::anyhow!("setup state encode failed: {e}")))?;
    cache_set(
        redis,
        &format!("{SETUP_STATE_PREFIX}{state}"),
        &payload,
        SETUP_STATE_TTL_SECS,
    )
    .await
    .map_err(AppError::Internal)?;
    Ok(state)
}

const SETUP_STATE_HOLDER_PREFIX: &str = "github:setup_state_holder:";

/// [`reserve_setup_state`] の結果。
#[derive(Debug)]
pub enum SetupStateReservation {
    /// この installation 用に予約できた（初回、または同じ installation の再試行）。
    Reserved(SetupState),
    /// 存在しない / 期限切れ / 消費済み。
    Unknown,
    /// 別の installation に予約済み。並行リクエストによる使い回しを弾いた。
    HeldByAnotherInstallation,
}

/// state を **1 つの installation に原子的に予約** してから読む。
///
/// GET → claim → GETDEL と分けると、予約前の state を並行リクエストが 2 本とも
/// 読めてしまい、別々の installation を claim できる。予約と読み取りを 1 つの
/// Lua スクリプトにまとめることで、最初に来た installation だけが使えるようにする。
///
/// 予約はここで消さない。webhook 到着待ちで claim は数回リトライされるため、
/// **同じ installation なら再利用でき**、削除は claim 成功時（[`consume_setup_state`]）に行う。
const RESERVE_SETUP_STATE_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
if not raw then
  return {0, ''}
end
local holder = redis.call('GET', KEYS[2])
if holder and holder ~= ARGV[1] then
  return {2, ''}
end
redis.call('SET', KEYS[2], ARGV[1], 'EX', ARGV[2])
return {1, raw}
"#;

pub async fn reserve_setup_state(
    redis: &RedisConnection,
    state: &str,
    installation_id: i64,
) -> Result<SetupStateReservation, AppError> {
    let mut conn = redis
        .conn
        .acquire()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("redis acquire failed: {e}")))?;

    let (outcome, raw): (i64, String) = redis::Script::new(RESERVE_SETUP_STATE_SCRIPT)
        .key(format!("{SETUP_STATE_PREFIX}{state}"))
        .key(format!("{SETUP_STATE_HOLDER_PREFIX}{state}"))
        .arg(installation_id)
        .arg(SETUP_STATE_TTL_SECS)
        .invoke_async(&mut *conn)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("reserve setup state failed: {e}")))?;

    match outcome {
        1 => match serde_json::from_str(&raw) {
            // 壊れた値は「無効な state」と同じ扱いにする（発行形式を変えた直後など）。
            Ok(parsed) => Ok(SetupStateReservation::Reserved(parsed)),
            Err(_) => Ok(SetupStateReservation::Unknown),
        },
        2 => Ok(SetupStateReservation::HeldByAnotherInstallation),
        _ => Ok(SetupStateReservation::Unknown),
    }
}

/// state 本体と予約キーを **1 つのスクリプトで消費** する。claim 成功後に呼ぶ。
///
/// GETDEL と予約キーの DEL を別コマンドに分けると、GETDEL 成功後に DEL だけ失敗した
/// とき「DB は claim 済み・API は 500・state は消費済みで再試行できない」という部分
/// 失敗が起きる。取得と両キー削除を 1 本の Lua にまとめて、両方消えるか一切消えないか
/// のどちらかにする。
///
/// 取得できなければ空文字列を返す（呼び出し側で `None` 扱い）。保存する値は必ず
/// JSON なので、空文字列を「存在しない」と見なしても本物の値と衝突しない。
const CONSUME_SETUP_STATE_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
redis.call('DEL', KEYS[1])
redis.call('DEL', KEYS[2])
if not raw then
  return ''
end
return raw
"#;

/// setup state を消費する（取得と両キー削除は原子的）。claim 成功後に呼ぶ。
///
/// 既に消えていれば `None`。予約キーも一緒に落として、TTL 分の残骸を残さない。
pub async fn consume_setup_state(
    redis: &RedisConnection,
    state: &str,
) -> Result<Option<SetupState>, AppError> {
    let mut conn = redis
        .conn
        .acquire()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("redis acquire failed: {e}")))?;

    let raw: String = redis::Script::new(CONSUME_SETUP_STATE_SCRIPT)
        .key(format!("{SETUP_STATE_PREFIX}{state}"))
        .key(format!("{SETUP_STATE_HOLDER_PREFIX}{state}"))
        .invoke_async(&mut *conn)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("consume setup state failed: {e}")))?;

    if raw.is_empty() {
        return Ok(None);
    }
    Ok(serde_json::from_str(&raw).ok())
}

#[derive(serde::Deserialize)]
struct InstallationRepositoriesPage {
    total_count: u64,
    repositories: Vec<GithubRepositoryResponse>,
}

/// Installation token で参照できるリポジトリを全ページ取得する。
///
/// GitHub API は 1 ページ最大 100 件なので、Organization 選択後の検索を UI 側で
/// 完結できるよう、空ページまたは `total_count` 到達までページングする。
pub async fn list_installation_repositories(
    redis: &RedisConnection,
    http: &reqwest::Client,
    settings: &Settings,
    installation_id: i64,
) -> Result<Vec<GithubRepositoryResponse>, GithubApiError> {
    let app = github_app(settings, http)
        .ok_or_else(|| GithubApiError::Permanent("github app is not configured".to_string()))?;
    let token = installation_token(redis, &app, installation_id).await?;
    let mut repositories = Vec::new();

    for page in 1..=MAX_REPOSITORY_PAGES {
        let url = format!(
            "{}/installation/repositories?per_page=100&page={page}",
            settings.github_api_base_url()
        );
        let response = http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| {
                GithubApiError::Transient(anyhow::anyhow!("list installation repositories: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let headers = response.headers().clone();
            let body: String = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            return Err(classify_failure(
                "list installation repositories",
                status,
                &headers,
                &body,
            ));
        }

        let body: InstallationRepositoriesPage = response.json().await.map_err(|e| {
            GithubApiError::Transient(anyhow::anyhow!("decode installation repositories: {e}"))
        })?;
        let total_count = body.total_count;
        let page_len = body.repositories.len();
        repositories.extend(body.repositories);
        if page_len == 0 || repositories.len() as u64 >= total_count {
            break;
        }
        if page == MAX_REPOSITORY_PAGES {
            // 途中までの一覧を正常結果として返すと「権限が無い」と区別できない。
            // 部分結果は捨てて、何件目で打ち切ったかが分かるエラーにする。
            return Err(GithubApiError::Permanent(format!(
                "list installation repositories exceeded the {MAX_REPOSITORY_PAGES} page limit: \
                 fetched {} of {total_count} repositories",
                repositories.len()
            )));
        }
    }

    repositories.sort_unstable_by(|left, right| {
        left.full_name
            .to_ascii_lowercase()
            .cmp(&right.full_name.to_ascii_lowercase())
    });
    Ok(repositories)
}

/// `owner/name` 形式のリポジトリ指定を検証する。
///
/// GitHub の owner / repo に使える文字だけを許可し、パストラバーサル
/// （`a/../b`）や空要素を弾く。
pub fn validate_repo(repo: &str) -> Result<(), common::error::AppError> {
    fn valid_segment(segment: &str) -> bool {
        !segment.is_empty()
            && segment.len() <= 100
            && segment != "."
            && segment != ".."
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    }

    let Some((owner, name)) = repo.split_once('/') else {
        return Err(common::error::AppError::BadRequestDetail(
            "github_repo must be in \"owner/name\" form".into(),
        ));
    };
    if !valid_segment(owner) || !valid_segment(name) {
        return Err(common::error::AppError::BadRequestDetail(
            "github_repo must be in \"owner/name\" form".into(),
        ));
    }
    Ok(())
}

/// ビルドの状態を GitHub のコミットステータスへ写像する。
///
/// `changes_detected` は **failure ではなく pending**。差分は人間のレビュー待ちであって、
/// それ自体が失敗ではない。レビューで approve / reject されたときに success / failure が付く。
pub fn status_for_build(build: &builds::Model) -> (CommitState, String) {
    use builds::BuildStatus::*;

    let changes = build.changed_count + build.added_count + build.removed_count;

    match build.status {
        Pending => (CommitState::Pending, "Waiting for screenshots".to_string()),
        Rendering => (
            CommitState::Pending,
            "Rendering stories from the Storybook bundle".to_string(),
        ),
        Processing => (
            CommitState::Pending,
            "Comparing screenshots against baseline".to_string(),
        ),
        Passed => (CommitState::Success, "Visual tests passed".to_string()),
        ChangesDetected => (
            CommitState::Pending,
            format!("{changes} {} detected, awaiting review", plural(changes)),
        ),
        Approved => (CommitState::Success, "Visual changes approved".to_string()),
        Rejected => (CommitState::Failure, "Visual changes rejected".to_string()),
        Failed => (CommitState::Error, "Visual test run failed".to_string()),
    }
}

fn plural(count: i32) -> &'static str {
    if count == 1 { "change" } else { "changes" }
}

/// レビュー UI のビルド詳細ページ（フロントエンドのルート形状は Phase 7）。
pub fn build_target_url(
    app_url: &str,
    tenant_slug: &str,
    project_slug: &str,
    number: i64,
) -> String {
    format!(
        "{}/t/{tenant_slug}/p/{project_slug}/builds/{number}",
        app_url.trim_end_matches('/')
    )
}

/// 同じ context の既存コミットステータスが、このプロジェクトのどのビルドを指しているか。
///
/// commit status には PR コメントのような不可視メタデータを埋める場所が無いので、
/// [`build_target_url`] が組み立てた `target_url`
/// （`/t/{tenant}/p/{project}/builds/{number}`）から読む。
///
/// context (`vrt`) はプロジェクトごとに変わらず、`projects.github_repo` にも一意制約が無い
/// （同じリポジトリに複数プロジェクトを紐付けられる）。ビルド番号はプロジェクトごとの
/// 独立した連番なので、別プロジェクトが書いたステータスの番号と比べても意味が無い。
/// そのため URL のテナント / プロジェクト slug が一致した場合だけ番号を返す。
///
/// 結合ステータス（`/commits/{sha}/status`）は context ごとの最新 1 件しか返さないので使わない。
/// 同じ repo に紐づく別プロジェクトが後から同じ context に書くと、自プロジェクトの最新が
/// 隠れて判定が素通りしてしまう（A#10 → B#1 → 遅延した A#9 で巻き戻る）。
/// 履歴を返す `/statuses` を新しい順に辿り、context と slug の両方が一致する最初の 1 件を使う。
///
/// 次のいずれでも `None` を返し、呼び出し側は判定を諦めて書き込む:
/// 一致するステータスが履歴に無い / URL から番号を読めない / [`MAX_STATUS_PAGES`] を超えた。
///
/// ponytail: GET と POST の間は不可分ではないので、GET の時点で観測できた巻き戻しだけを
/// 防ぐ。窓を閉じるなら SHA 単位の直列化が要る。
#[allow(clippy::too_many_arguments)]
pub async fn latest_status_build_number(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    repo: &str,
    sha: &str,
    context: &str,
    tenant_slug: &str,
    project_slug: &str,
) -> Result<Option<i64>, GithubApiError> {
    #[derive(serde::Deserialize)]
    struct CommitStatus {
        context: String,
        #[serde(default)]
        target_url: Option<String>,
    }

    for page in 1..=MAX_STATUS_PAGES {
        let url = format!(
            "{}/repos/{repo}/commits/{sha}/statuses?per_page=100&page={page}",
            base_url.trim_end_matches('/')
        );
        let response = http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| GithubApiError::Transient(anyhow::anyhow!("list commit statuses: {e}")))?;

        if !response.status().is_success() {
            return Err(error_from_response("list commit statuses", response).await);
        }

        let statuses: Vec<CommitStatus> = response.json().await.map_err(|e| {
            GithubApiError::Transient(anyhow::anyhow!("decode commit statuses: {e}"))
        })?;

        // 一覧は新しい順で返る。最初に一致したものが自プロジェクトの最新ステータス。
        let page_len = statuses.len();
        if let Some(number) = statuses
            .into_iter()
            .filter(|s| s.context == context)
            .filter_map(|s| s.target_url)
            .find_map(|url| {
                let (tenant, project, number) = parse_target_url_build(&url)?;
                (tenant == tenant_slug && project == project_slug).then_some(number)
            })
        {
            return Ok(Some(number));
        }
        if page_len < 100 {
            return Ok(None);
        }
    }
    // ponytail: ページ上限到達時は判定を諦めて書き込む。他 CI が同じ SHA に数百件書く repo で
    // だけ起きる話で、そのときの挙動は #23 以前（無条件に書き込む）と同じ。
    Ok(None)
}

/// [`latest_status_build_number`] が遡るページ数の上限（`/statuses` は 100 件/ページ）。
const MAX_STATUS_PAGES: u32 = 10;

/// `.../t/{tenant}/p/{project}/builds/{number}` を分解する（[`build_target_url`] の逆）。
fn parse_target_url_build(target_url: &str) -> Option<(&str, &str, i64)> {
    let (head, number) = target_url.trim_end_matches('/').rsplit_once("/builds/")?;
    let (head, project) = head.rsplit_once("/p/")?;
    let (_, tenant) = head.rsplit_once("/t/")?;
    Some((tenant, project, number.parse().ok()?))
}

/// `POST /repos/{repo}/statuses/{sha}` でコミットステータスを作成する。
///
/// `repo` は `owner/name`。`token` は Installation Access Token。
#[allow(clippy::too_many_arguments)]
pub async fn post_commit_status(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    repo: &str,
    sha: &str,
    state: CommitState,
    description: &str,
    target_url: Option<&str>,
    context: &str,
) -> Result<(), GithubApiError> {
    // GitHub の description は 140 文字上限。
    let description: String = description.chars().take(140).collect();

    let mut body = serde_json::json!({
        "state": state.as_str(),
        "description": description,
        "context": context,
    });
    if let Some(url) = target_url {
        body["target_url"] = serde_json::Value::String(url.to_string());
    }

    let url = format!(
        "{}/repos/{repo}/statuses/{sha}",
        base_url.trim_end_matches('/')
    );

    let response = http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .json(&body)
        .send()
        .await
        .map_err(|e| GithubApiError::Transient(anyhow::anyhow!("post commit status: {e}")))?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    let headers = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    let body: String = body.chars().take(500).collect();
    Err(classify_failure(
        "post commit status",
        status,
        &headers,
        &body,
    ))
}

// ── PR コメント（ビルドリンク）──────────────────────────────────────────────
//
// Chromatic のように、ビルドの状態とレビュー UI へのリンクを PR のコメントとして
// 掲示する。ビルドの状態遷移ごとに新しいコメントを積むとうるさいので、
// 不可視マーカーで自分のコメントを見つけて更新する（無ければ作成）。

/// PR コメントを走査するページ数の上限（1 ページ 100 件）。
///
/// コメント 1,000 件超の PR で自分のコメントを見つけられなくても、コメントは
/// 補助表示なので諦めて新規作成にフォールバックする（リポジトリ一覧と違って
/// エラーにはしない）。
const MAX_COMMENT_PAGES: u32 = 10;

/// 自分のコメントを識別する不可視マーカー。
///
/// プロジェクト単位にすることで、複数プロジェクトが同じ PR に報告しても
/// 互いのコメントを上書きしない。
pub fn pr_comment_marker(project_id: Uuid) -> String {
    format!("<!-- vrt:{project_id} -->")
}

/// コメントが指しているビルド番号を保持する不可視メタデータ。
const PR_COMMENT_BUILD_NUMBER_PREFIX: &str = "<!-- vrt:build_number:";

fn pr_comment_build_number_metadata(build_number: i64) -> String {
    format!("{PR_COMMENT_BUILD_NUMBER_PREFIX}{build_number} -->")
}

/// 既存コメントから読み取ったビルド番号メタデータの状態。
///
/// 「メタデータが無い」（#22 以前に作られたコメント。移行のため素通しする）と
/// 「メタデータはあるが読めない」を区別するために 3 状態にしている。
/// どちらも更新は通すが、後者は想定外なのでログに残す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommentBuildNumber {
    Missing,
    Malformed,
    Present(i64),
}

fn parse_pr_comment_build_number(body: &str) -> CommentBuildNumber {
    let Some(value) = body.lines().find_map(|line| {
        line.trim()
            .strip_prefix(PR_COMMENT_BUILD_NUMBER_PREFIX)?
            .strip_suffix(" -->")
    }) else {
        return CommentBuildNumber::Missing;
    };
    match value.parse() {
        Ok(number) => CommentBuildNumber::Present(number),
        Err(_) => CommentBuildNumber::Malformed,
    }
}

/// PR コメントの本文を組み立てる。
///
/// `description` は [`status_for_build`] の文言をそのまま使う。
pub fn pr_comment_body(
    marker: &str,
    project_slug: &str,
    build_number: i64,
    description: &str,
    target_url: &str,
) -> String {
    let build_number_metadata = pr_comment_build_number_metadata(build_number);
    format!(
        "{marker}\n{build_number_metadata}\n## 📸 VRT — {project_slug} build #{build_number}\n\n\
         **{description}**\n\n[View build]({target_url})\n"
    )
}

#[derive(serde::Deserialize)]
struct IssueComment {
    id: i64,
    #[serde(default)]
    body: Option<String>,
}

/// 失敗レスポンスをヘッダ・本文込みで [`GithubApiError`] に変換する。
async fn error_from_response(context: &str, response: reqwest::Response) -> GithubApiError {
    let status = response.status();
    let headers = response.headers().clone();
    let body: String = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(500)
        .collect();
    classify_failure(context, status, &headers, &body)
}

/// マーカーを含む既存コメントを PR から探す。見つからなければ `None`。
async fn find_marker_comment(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    repo: &str,
    pr_number: i32,
    marker: &str,
) -> Result<Option<IssueComment>, GithubApiError> {
    for page in 1..=MAX_COMMENT_PAGES {
        let url = format!(
            "{}/repos/{repo}/issues/{pr_number}/comments?per_page=100&page={page}",
            base_url.trim_end_matches('/')
        );
        let response = http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| GithubApiError::Transient(anyhow::anyhow!("list pr comments: {e}")))?;

        if !response.status().is_success() {
            return Err(error_from_response("list pr comments", response).await);
        }

        let comments: Vec<IssueComment> = response
            .json()
            .await
            .map_err(|e| GithubApiError::Transient(anyhow::anyhow!("decode pr comments: {e}")))?;

        // 一覧は古い順で返る。マーカーを引用した他人のコメントより先に、
        // 自分の（最初に作った）コメントが必ずヒットする。
        let page_len = comments.len();
        if let Some(comment) = comments
            .into_iter()
            .find(|c| c.body.as_deref().is_some_and(|b| b.contains(marker)))
        {
            return Ok(Some(comment));
        }
        if page_len < 100 {
            return Ok(None);
        }
    }
    // ponytail: ページ上限到達時は「見つからなかった」扱いで新規作成に倒す。
    // 巨大 PR で重複コメントが出うるが、コメントは補助表示なので許容する。
    Ok(None)
}

/// 書き込んだか、古いジョブとしてスキップしたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentWrite {
    Wrote,
    /// 既存コメントがより新しいビルドを指していたのでスキップした。
    SkippedStale {
        existing_build_number: i64,
    },
}

/// PR のビルドリンクコメントを作成または更新する。
///
/// `marker` を含む既存コメントがあれば `PATCH` で本文を差し替え、
/// 無ければ `POST /repos/{repo}/issues/{pr_number}/comments` で作成する。
/// 既存コメントの不可視メタデータがより新しいビルド番号を指している場合は、
/// 遅延・再試行された古いジョブによる巻き戻しを防ぐため更新しない。
/// 同一ビルドの更新は通す（1 ビルドにつき finalize / 比較完了 / approve・reject と
/// 複数回ジョブが走り、そのたびに description が変わるため）。
///
/// ponytail: 防げるのは GET の時点で観測できた巻き戻しだけで、GET と PATCH の間に
/// 新しいジョブが書き込んだ場合は検知できない（issue comment API に条件付き更新が無い）。
/// 同じ理由で、同一 PR のジョブが並行すると両方がマーカーを見つけられず二重投稿にも
/// なりうる。窓を実際に閉じるなら PR 単位でジョブを直列化することになる。
#[allow(clippy::too_many_arguments)]
pub async fn upsert_pr_comment(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    repo: &str,
    pr_number: i32,
    marker: &str,
    build_number: i64,
    body: &str,
) -> Result<CommentWrite, GithubApiError> {
    let existing = find_marker_comment(http, base_url, token, repo, pr_number, marker).await?;

    let base = base_url.trim_end_matches('/');
    let request = match existing {
        Some(comment) => {
            match comment
                .body
                .as_deref()
                .map_or(CommentBuildNumber::Missing, parse_pr_comment_build_number)
            {
                CommentBuildNumber::Present(existing_build_number)
                    if existing_build_number > build_number =>
                {
                    return Ok(CommentWrite::SkippedStale {
                        existing_build_number,
                    });
                }
                CommentBuildNumber::Malformed => {
                    tracing::warn!(
                        repo,
                        pr_number,
                        comment_id = comment.id,
                        "pr comment has unparsable build number metadata; updating anyway"
                    );
                }
                _ => {}
            }
            http.patch(format!(
                "{base}/repos/{repo}/issues/comments/{}",
                comment.id
            ))
        }
        None => http.post(format!("{base}/repos/{repo}/issues/{pr_number}/comments")),
    };

    let response = request
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .json(&serde_json::json!({ "body": body }))
        .send()
        .await
        .map_err(|e| GithubApiError::Transient(anyhow::anyhow!("upsert pr comment: {e}")))?;

    if response.status().is_success() {
        return Ok(CommentWrite::Wrote);
    }
    Err(error_from_response("upsert pr comment", response).await)
}

// ── installations（claim とプロジェクト紐付け）──────────────────────────────

use common::error::AppError;
use entity::{github_installations, projects};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, prelude::Uuid,
};

/// テナントが claim 済みの installation 一覧（アンインストール済みは除く）。
pub async fn list_installations_for_tenant<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Vec<github_installations::Model>, AppError> {
    Ok(github_installations::Entity::find()
        .filter(github_installations::Column::TenantId.eq(tenant_id))
        .filter(github_installations::Column::DeletedAt.is_null())
        .order_by_asc(github_installations::Column::CreatedAt)
        .all(db)
        .await?)
}

/// まだどのテナントにも claim されていない installation 一覧。
///
/// SECURITY(MVP): ログイン済みユーザーなら誰でも「未 claim の installation の
/// アカウント名」を一覧できる。これは「App をインストールした直後の人が、
/// 自分のテナントに紐付ける」導線を成立させるための割り切りで、
/// 見えるのはアカウント名と種別のみ、claim 自体は先着 1 テナント（[`claim_installation`]）。
/// 将来 setup_url + 短命トークンで「インストールした本人だけが見える」形に絞る。
pub async fn list_unclaimed_installations<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<github_installations::Model>, AppError> {
    Ok(github_installations::Entity::find()
        .filter(github_installations::Column::TenantId.is_null())
        .filter(github_installations::Column::DeletedAt.is_null())
        .order_by_asc(github_installations::Column::CreatedAt)
        .all(db)
        .await?)
}

/// GitHub の installation ID で行を引く（アンインストール済みは除く）。
pub async fn find_installation<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
) -> Result<Option<github_installations::Model>, AppError> {
    Ok(github_installations::Entity::find()
        .filter(github_installations::Column::InstallationId.eq(installation_id))
        .filter(github_installations::Column::DeletedAt.is_null())
        .one(db)
        .await?)
}

/// installation をテナントに紐付ける。
///
/// - 未 claim → claim する
/// - 既に同じテナントが claim 済み → 冪等に成功
/// - 他テナントが claim 済み → [`AppError::Conflict`]
pub async fn claim_installation<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
    tenant_id: Uuid,
) -> Result<github_installations::Model, AppError> {
    let model = find_installation(db, installation_id)
        .await?
        .ok_or(AppError::NotFound)?;

    match model.tenant_id {
        Some(existing) if existing == tenant_id => return Ok(model),
        Some(_) => return Err(AppError::Conflict),
        None => {}
    }

    let mut active: github_installations::ActiveModel = model.into();
    active.tenant_id = Set(Some(tenant_id));
    active.updated_at = Set(chrono::Utc::now().fixed_offset());
    Ok(active.update(db).await?)
}

/// プロジェクトの GitHub 連携を設定 / 解除する。
///
/// `link` が `None` なら解除。`Some((installation_id, repo))` なら、その installation が
/// **プロジェクトと同じテナントのもの**であることを確認してから紐付ける。
pub async fn set_project_github_link<C: ConnectionTrait>(
    db: &C,
    project: projects::Model,
    link: Option<(i64, String)>,
) -> Result<projects::Model, AppError> {
    let (installation_id, repo) = match link {
        None => {
            let mut active: projects::ActiveModel = project.into();
            active.github_installation_id = Set(None);
            active.github_repo = Set(None);
            active.updated_at = Set(chrono::Utc::now().fixed_offset());
            return Ok(active.update(db).await?);
        }
        Some(link) => link,
    };

    validate_repo(&repo)?;

    let installation = find_installation(db, installation_id)
        .await?
        .ok_or_else(|| AppError::BadRequestDetail("unknown github installation".into()))?;

    // 他テナントの installation を借りてステータスを書き込めないようにする。
    if installation.tenant_id != Some(project.tenant_id) {
        return Err(AppError::Forbidden);
    }

    let mut active: projects::ActiveModel = project.into();
    active.github_installation_id = Set(Some(installation_id));
    active.github_repo = Set(Some(repo));
    active.updated_at = Set(chrono::Utc::now().fixed_offset());
    Ok(active.update(db).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn build(status: builds::BuildStatus, changed: i32, added: i32, removed: i32) -> builds::Model {
        builds::Model {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            number: 1,
            branch: "main".into(),
            commit_sha: "a".repeat(40),
            commit_message: None,
            pull_request_number: None,
            status,
            mode: builds::BuildMode::Screenshots,
            storybook_key: None,
            baseline_id: None,
            capture_plan: None,
            total_count: 0,
            changed_count: changed,
            added_count: added,
            removed_count: removed,
            unchanged_count: 0,
            content_hash_skipped_count: 0,
            error_message: None,
            approval_evidence: None,
            approved_by: None,
            approved_at: None,
            created_at: Utc::now().fixed_offset(),
            completed_at: None,
        }
    }

    #[test]
    fn maps_build_status_to_commit_state() {
        use builds::BuildStatus::*;
        assert_eq!(
            status_for_build(&build(Processing, 0, 0, 0)).0,
            CommitState::Pending
        );
        assert_eq!(
            status_for_build(&build(Passed, 0, 0, 0)).0,
            CommitState::Success
        );
        // 差分検出はレビュー待ち = pending。failure にはしない。
        assert_eq!(
            status_for_build(&build(ChangesDetected, 1, 0, 0)).0,
            CommitState::Pending
        );
        assert_eq!(
            status_for_build(&build(Approved, 1, 0, 0)).0,
            CommitState::Success
        );
        assert_eq!(
            status_for_build(&build(Rejected, 1, 0, 0)).0,
            CommitState::Failure
        );
        assert_eq!(
            status_for_build(&build(Failed, 0, 0, 0)).0,
            CommitState::Error
        );
    }

    #[test]
    fn changes_detected_description_counts_all_difference_kinds() {
        let (_, description) =
            status_for_build(&build(builds::BuildStatus::ChangesDetected, 2, 1, 1));
        assert_eq!(description, "4 changes detected, awaiting review");

        let (_, singular) = status_for_build(&build(builds::BuildStatus::ChangesDetected, 1, 0, 0));
        assert_eq!(singular, "1 change detected, awaiting review");
    }

    #[test]
    fn target_url_uses_frontend_build_route() {
        assert_eq!(
            build_target_url("https://vrt.example.com/", "acme", "web", 42),
            "https://vrt.example.com/t/acme/p/web/builds/42"
        );
    }

    #[test]
    fn pr_comment_body_embeds_marker_and_link() {
        let marker = pr_comment_marker(Uuid::nil());
        assert_eq!(marker, "<!-- vrt:00000000-0000-0000-0000-000000000000 -->");

        let body = pr_comment_body(
            &marker,
            "web",
            42,
            "4 changes detected, awaiting review",
            "https://vrt.example.com/t/acme/p/web/builds/42",
        );
        assert!(
            body.starts_with(&marker),
            "マーカーが先頭にあること: {body}"
        );
        assert!(body.contains("<!-- vrt:build_number:42 -->"));
        assert_eq!(
            parse_pr_comment_build_number(&body),
            CommentBuildNumber::Present(42)
        );
        assert!(body.contains("web build #42"));
        assert!(body.contains("4 changes detected, awaiting review"));
        assert!(body.contains("https://vrt.example.com/t/acme/p/web/builds/42"));
    }

    #[test]
    fn distinguishes_missing_and_malformed_build_number_metadata() {
        assert_eq!(
            parse_pr_comment_build_number("<!-- vrt:proj -->\n## VRT\n"),
            CommentBuildNumber::Missing
        );
        assert_eq!(
            parse_pr_comment_build_number("<!-- vrt:build_number:oops -->"),
            CommentBuildNumber::Malformed
        );
    }

    #[test]
    fn reads_tenant_project_and_build_number_back_from_target_url() {
        let url = build_target_url("https://vrt.example.com", "acme", "web", 42);
        assert_eq!(parse_target_url_build(&url), Some(("acme", "web", 42)));
        assert_eq!(
            parse_target_url_build(&format!("{url}/")),
            Some(("acme", "web", 42))
        );
        assert_eq!(
            parse_target_url_build("https://vrt.example.com/t/acme/p/web"),
            None
        );
        assert_eq!(
            parse_target_url_build("https://vrt.example.com/t/acme/p/web/builds/x"),
            None
        );
    }

    #[test]
    fn accepts_well_formed_repositories() {
        assert!(validate_repo("octocat/hello-world").is_ok());
        assert!(validate_repo("acme_inc/web.app").is_ok());
    }

    #[test]
    fn rejects_malformed_repositories() {
        assert!(validate_repo("octocat").is_err());
        assert!(validate_repo("octocat/").is_err());
        assert!(validate_repo("/hello").is_err());
        assert!(validate_repo("octocat/hello/world").is_err());
        assert!(validate_repo("octocat/..").is_err());
        assert!(validate_repo("oct at/hello").is_err());
    }

    #[test]
    fn github_app_is_none_when_unconfigured() {
        // 設定が欠けているときは連携を無効にするだけで、起動は止めない。
        let http = reqwest::Client::new();
        let mut settings = common::settings::Settings {
            database_url: String::new(),
            redis_url: String::new(),
            allow_origin: String::new(),
            listen_addr: String::new(),
            app_url: "http://localhost:3000".into(),
            personal_token_secret: "a".repeat(32),
            oauth_token_encryption_key: "b".repeat(32),
            github_client_id: String::new(),
            github_client_secret: String::new(),
            gitlab_client_id: String::new(),
            gitlab_client_secret: String::new(),
            gitlab_instance_url: None,
            github_app_id: None,
            github_app_private_key_pem: None,
            github_app_install_url: None,
            github_webhook_secret: None,
            github_api_base_url: None,
            storage_backend: "local".into(),
            local_upload_dir: "./uploads".into(),
            storybook_cache_dir: "./storybook-cache".into(),
            s3_endpoint: None,
            s3_bucket: None,
            s3_region: None,
            s3_access_key_id: None,
            s3_secret_access_key: None,
            s3_public_base_url: None,
            s3_force_path_style: None,
            chromium_path: None,
            test_login_enabled: false,
        };
        assert!(github_app(&settings, &http).is_none());

        // App ID だけでは足りない。
        settings.github_app_id = Some(123);
        assert!(github_app(&settings, &http).is_none());

        settings.github_app_private_key_pem = Some("pem".into());
        assert!(github_app(&settings, &http).is_some());
    }

    #[test]
    fn api_base_url_defaults_to_github_and_trims_trailing_slash() {
        let mut settings = common::settings::Settings {
            database_url: String::new(),
            redis_url: String::new(),
            allow_origin: String::new(),
            listen_addr: String::new(),
            app_url: "http://localhost:3000".into(),
            personal_token_secret: "a".repeat(32),
            oauth_token_encryption_key: "b".repeat(32),
            github_client_id: String::new(),
            github_client_secret: String::new(),
            gitlab_client_id: String::new(),
            gitlab_client_secret: String::new(),
            gitlab_instance_url: None,
            github_app_id: None,
            github_app_private_key_pem: None,
            github_app_install_url: None,
            github_webhook_secret: None,
            github_api_base_url: None,
            storage_backend: "local".into(),
            local_upload_dir: "./uploads".into(),
            storybook_cache_dir: "./storybook-cache".into(),
            s3_endpoint: None,
            s3_bucket: None,
            s3_region: None,
            s3_access_key_id: None,
            s3_secret_access_key: None,
            s3_public_base_url: None,
            s3_force_path_style: None,
            chromium_path: None,
            test_login_enabled: false,
        };
        assert_eq!(settings.github_api_base_url(), "https://api.github.com");
        settings.github_api_base_url = Some("http://127.0.0.1:8080/".into());
        assert_eq!(settings.github_api_base_url(), "http://127.0.0.1:8080");
    }
}
