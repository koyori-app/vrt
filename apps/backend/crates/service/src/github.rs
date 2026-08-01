//! GitHub App 連携（PR コミットステータス）。
//!
//! - App 資格情報の組み立ては [`github_app`]。設定が無ければ `None` を返し、
//!   呼び出し側は「機能無効」として素通りする（起動は止めない）。
//! - Installation Access Token は Valkey にキャッシュする（[`installation_token`]）。
//!   GitHub のトークン有効期限は 1 時間なので、少し短い 50 分を TTL にする。
//! - コミットステータスの POST は [`post_commit_status`]。
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

/// コミットステータスの `context`（GitHub の PR チェック一覧に出る名前）。
pub const STATUS_CONTEXT: &str = "vrt";

/// Installation Access Token のキャッシュ TTL（秒）。
/// GitHub 側の有効期限は 1 時間なので、期限ぎりぎりのトークンを掴まないよう短めにする。
pub const INSTALLATION_TOKEN_TTL_SECS: u64 = 50 * 60;

/// GitHub API 呼び出しの User-Agent（GitHub は UA 必須）。
const USER_AGENT: &str = "vrt";

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

    let body = response.text().await.unwrap_or_default();
    let body: String = body.chars().take(500).collect();
    if status.is_client_error() {
        Err(GithubApiError::Permanent(format!(
            "post commit status failed: {status} {body}"
        )))
    } else {
        Err(GithubApiError::Transient(anyhow::anyhow!(
            "post commit status failed: {status} {body}"
        )))
    }
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
            error_message: None,
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
