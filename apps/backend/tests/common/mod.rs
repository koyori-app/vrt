//! HTTP 統合テスト用の Axum アプリ構築ヘルパー（task の tests/common/mod.rs を移植）。
//!
//! - Postgres / Valkey は `DATABASE_URL` / `REDIS_URL` が無ければ testcontainers で
//!   ランダムポートに起動する
//! - アプリはランダムポートにインプロセスで spawn し、reqwest（Cookie ストア付き）で叩く
//! - OAuth プロバイダーは wiremock で偽装する

// 各統合テストバイナリごとにコンパイルされるため、一部バイナリで未使用のヘルパーが
// dead_code 誤検知になるのを抑止する。
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use axum::{Router, middleware};
use axum_session::{SameSite, SessionConfig, SessionLayer, SessionMode, SessionStore};
use axum_session_redispool::SessionRedisPool;
use backend::{
    AppState,
    jobs::{
        setup_compare_build_storage_with_queue, setup_github_status_storage_with_queue,
        setup_github_webhook_storage_with_queue, setup_pool,
    },
    routes, settings,
};
use common::cache::redis::RedisConnection;
use cookie::Key;
use entity::{oauth_connections, personal_tokens, scopes::Scope, users};
use reqwest::{Client, Response, StatusCode, header, redirect::Policy};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use serde_json::json;
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{
        Container, GenericImage, ImageExt,
        core::{IntoContainerPort, WaitFor},
        runners::SyncRunner,
    },
};
use tokio::{net::TcpListener, sync::OnceCell, sync::watch};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, path_regex},
};

static SCHEMA_READY: OnceCell<()> = OnceCell::const_new();
static TEST_ENV_READY: OnceLock<()> = OnceLock::new();

pub const TEST_OAUTH_CLIENT_ID: &str = "test-gitlab-client";
pub const TEST_OAUTH_CLIENT_SECRET: &str = "test-gitlab-secret";
pub const TEST_ENCRYPTION_KEY: &str = "01234567890123456789012345678901";
/// wiremock で偽装した GitLab を指す（`GitlabSelfHostedProvider` の slug）。
pub const TEST_PROVIDER: &str = "gitlab_selfhosted";
pub const MOCK_ACCESS_TOKEN: &str = "mock-access-token";

/// GitHub の `X-Hub-Signature-256` を計算する（`sha256=<hex>`）。
pub fn sign_webhook(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn init_tracing() {
    static TRACING: OnceLock<()> = OnceLock::new();
    TRACING.get_or_init(|| {
        let _ =
            tracing_subscriber::fmt()
                .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
                    "backend=debug,handler=debug,service=debug,job=debug".into()
                }))
                .with_test_writer()
                .try_init();
    });
}

pub fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

pub fn location_of(response: &Response) -> String {
    response
        .headers()
        .get(header::LOCATION)
        .expect("location header")
        .to_str()
        .expect("location utf8")
        .to_string()
}

/// テストバイナリのプロセス生存期間中コンテナを保持する static。
/// testcontainers-rs 0.27 に ryuk（外部リーパー）は無く、削除は `Container` の
/// Drop が担うため、`drop_test_containers`（atexit）で明示的に drop する。
static PG_CONTAINER: Mutex<Option<Container<Postgres>>> = Mutex::new(None);
static VALKEY_CONTAINER: Mutex<Option<Container<GenericImage>>> = Mutex::new(None);

extern "C" fn drop_test_containers() {
    if let Ok(mut guard) = PG_CONTAINER.lock() {
        guard.take();
    }
    if let Ok(mut guard) = VALKEY_CONTAINER.lock() {
        guard.take();
    }
}

fn start_test_containers(need_pg: bool, need_redis: bool) -> (Option<String>, Option<String>) {
    // SAFETY: 登録するのはキャプチャ無しの extern "C" 関数で、libc の規約どおり。
    unsafe {
        libc::atexit(drop_test_containers);
    }
    let pg_url = need_pg.then(|| {
        // テストは 1 プロセスで多数のアプリインスタンスを立てるため、
        // 既定の max_connections=100 では足りない。上限を明示的に広げておく。
        let container = Postgres::default()
            .with_tag("17")
            .with_cmd(["postgres", "-c", "max_connections=500"])
            .start()
            .expect("start postgres testcontainer");
        let port = container
            .get_host_port_ipv4(5432)
            .expect("postgres host port");
        *PG_CONTAINER.lock().expect("pg container slot") = Some(container);
        format!("postgresql://postgres:postgres@127.0.0.1:{port}/postgres")
    });
    let redis_url = need_redis.then(|| {
        let container = GenericImage::new("valkey/valkey", "8.1")
            .with_exposed_port(6379.tcp())
            .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
            .start()
            .expect("start valkey testcontainer");
        let port = container
            .get_host_port_ipv4(6379)
            .expect("valkey host port");
        *VALKEY_CONTAINER.lock().expect("valkey container slot") = Some(container);
        format!("redis://127.0.0.1:{port}")
    });
    (pg_url, redis_url)
}

fn set_default_env(key: &str, value: &str) {
    if std::env::var(key).is_err() {
        // SAFETY: test process is single-threaded before Tokio workers spawn.
        unsafe { std::env::set_var(key, value) };
    }
}

fn ensure_test_env() {
    TEST_ENV_READY.get_or_init(|| {
        let _ = dotenvy::dotenv().ok();

        let need_pg = std::env::var("DATABASE_URL").is_err();
        let need_redis = std::env::var("REDIS_URL").is_err();
        if need_pg || need_redis {
            // SyncRunner は内部で tokio ランタイムを使うため、テストのランタイム上から
            // 直接呼ぶとパニックする。専用スレッドで起動して結果だけ受け取る。
            let (pg_url, redis_url) =
                std::thread::spawn(move || start_test_containers(need_pg, need_redis))
                    .join()
                    .expect("start test containers");
            // SAFETY: test process is single-threaded before Tokio workers spawn.
            unsafe {
                if let Some(url) = pg_url {
                    std::env::set_var("DATABASE_URL", url);
                }
                if let Some(url) = redis_url {
                    std::env::set_var("REDIS_URL", url);
                }
            }
        }

        // 1 テストバイナリで最大 9 個の TestApp（= アプリ実体）が同時に生きる。
        // 各 TestApp は「SeaORM プール + apalis プール」を持ち、apalis ワーカーが
        // 常時ポーリングして接続を掴むため、既定(10+10)のままだと
        // Postgres の max_connections を超えて PoolTimedOut になる。
        //
        // apalis 側は「ワーカー数（compare_build / github_status / github_webhook の 3 本）」を
        // 下回らせない。2 本のままだと 3 ワーカーが 1 本を奪い合ってフェッチが詰まる。
        set_default_env("DATABASE_MAX_CONNECTIONS", "5");
        set_default_env("APALIS_MAX_CONNECTIONS", "3");

        set_default_env("APP_URL", "http://localhost:3000");
        set_default_env("ALLOW_ORIGIN", "http://localhost:3000");
        set_default_env("PERSONAL_TOKEN_SECRET", TEST_ENCRYPTION_KEY);
        set_default_env("OAUTH_TOKEN_ENCRYPTION_KEY", TEST_ENCRYPTION_KEY);
        set_default_env("GITHUB_CLIENT_ID", "test-github-client");
        set_default_env("GITHUB_CLIENT_SECRET", "test-github-secret");
        set_default_env("GITLAB_CLIENT_ID", TEST_OAUTH_CLIENT_ID);
        set_default_env("GITLAB_CLIENT_SECRET", TEST_OAUTH_CLIENT_SECRET);
        set_default_env(
            "LOCAL_UPLOAD_DIR",
            std::env::temp_dir()
                .join("vrt-test-uploads")
                .to_str()
                .expect("temp dir utf8"),
        );
    });
}

async fn ensure_schema(db: &DatabaseConnection) {
    SCHEMA_READY
        .get_or_init(|| async {
            db.get_schema_registry("entity::*")
                .sync(db)
                .await
                .expect("sync schema");
        })
        .await;
}

fn test_session_config() -> SessionConfig {
    SessionConfig::default()
        .with_secure(false)
        .with_cookie_same_site(SameSite::Lax)
        .with_ip_and_user_agent(false)
        .with_prefix_with_host(false)
        .with_mode(SessionMode::Persistent)
        .with_key(Key::from(&[7u8; 64]))
        .with_database_key(Key::from(&[8u8; 64]))
}

/// wiremock で偽装した GitLab インスタンス。
///
/// `GitlabSelfHostedProvider` はインスタンス URL から
/// `/oauth/token` と `/api/v4/user` を組み立てるため、この 2 本だけ用意すればよい。
pub struct MockProvider {
    pub server: MockServer,
    pub user_id: i64,
    pub username: String,
    pub email: String,
}

impl MockProvider {
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        // oauth_connections は (provider, provider_user_id) が UNIQUE。並列テストが
        // 同じ ID を使うと 2 本目が 1 本目のユーザーに紐付いてしまうため毎回変える。
        let unique = Uuid::new_v4();
        let user_id = i64::from(u32::from_be_bytes(
            unique.as_bytes()[..4].try_into().unwrap(),
        ));
        let username = format!("oauth-user-{}", &unique.to_string()[..8]);
        let email = format!("{username}@example.com");

        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": MOCK_ACCESS_TOKEN,
                "token_type": "Bearer",
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": user_id,
                "username": username,
                "email": email,
                "confirmed_at": "2024-01-01T00:00:00Z",
                "avatar_url": null,
            })))
            .mount(&server)
            .await;

        Self {
            server,
            user_id,
            username,
            email,
        }
    }

    pub fn instance_url(&self) -> String {
        self.server.uri()
    }
}

// ── GitHub App（Phase 6）のテスト設定 ────────────────────────────────────────

/// GitHub App の秘密鍵として使う使い捨ての RSA 鍵（テスト専用・本物ではない）。
///
/// `forge-github` は installation token を取りに行く前に必ず RS256 の App JWT を
/// 発行するため、wiremock を相手にする場合でも **構文的に正しい RSA 秘密鍵**が要る。
pub const TEST_GITHUB_APP_PRIVATE_KEY: &str = include_str!("../fixtures/github_app_test_key.pem");
pub const TEST_GITHUB_APP_ID: u64 = 424242;
pub const TEST_GITHUB_WEBHOOK_SECRET: &str = "test-github-webhook-secret";
/// wiremock が返す installation access token。
pub const TEST_INSTALLATION_TOKEN: &str = "ghs_mock_installation_token";

/// GitHub API を偽装する wiremock サーバー。
///
/// `POST /app/installations/{id}/access_tokens` だけを最初から用意しておく。
/// コミットステータスの受け口はテスト側が必要に応じて mount する。
pub struct MockGithub {
    pub server: MockServer,
}

impl MockGithub {
    pub async fn start() -> Self {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"^/app/installations/\d+/access_tokens$"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "token": TEST_INSTALLATION_TOKEN,
                // forge-github は RFC3339 としてパースする。
                "expires_at": "2099-01-01T00:00:00Z",
            })))
            .mount(&server)
            .await;

        Self { server }
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    /// `POST /repos/{repo}/statuses/{sha}` を受け付けて 201 を返す。
    pub async fn expect_commit_statuses(&self, repo: &str, sha: &str) {
        Mock::given(method("POST"))
            .and(path(format!("/repos/{repo}/statuses/{sha}")))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .mount(&self.server)
            .await;
    }

    /// これまでに受けた `POST /repos/{repo}/statuses/{sha}` のリクエストを返す。
    pub async fn status_requests(&self, repo: &str, sha: &str) -> Vec<wiremock::Request> {
        let target = format!("/repos/{repo}/statuses/{sha}");
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|req| req.method == wiremock::http::Method::POST && req.url.path() == target)
            .collect()
    }

    /// 指定 state のステータス POST が届くまで待ち、そのボディを返す。
    pub async fn wait_for_status(
        &self,
        repo: &str,
        sha: &str,
        state: &str,
        timeout: std::time::Duration,
    ) -> serde_json::Value {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            for req in self.status_requests(repo, sha).await {
                let body: serde_json::Value = match serde_json::from_slice(&req.body) {
                    Ok(body) => body,
                    Err(_) => continue,
                };
                if body["state"].as_str() == Some(state) {
                    return body;
                }
            }
            if std::time::Instant::now() >= deadline {
                let seen: Vec<String> = self
                    .status_requests(repo, sha)
                    .await
                    .iter()
                    .filter_map(|req| serde_json::from_slice::<serde_json::Value>(&req.body).ok())
                    .map(|body| body["state"].as_str().unwrap_or("?").to_string())
                    .collect();
                panic!(
                    "no `{state}` commit status for {repo}@{sha} within {timeout:?} (seen: {seen:?})"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

/// `TestApp` の組み立てオプション。
#[derive(Default)]
pub struct TestAppOptions {
    /// `Some` なら GitHub App を有効化し、API のベース URL をこの wiremock に向ける。
    pub github: Option<MockGithub>,
    /// e2e 用のテスト専用ログイン口を開くか（既定 false = 本番と同じく 404）。
    pub test_login: bool,
}

pub struct TestApp {
    pub state: AppState,
    pub base_url: String,
    pub provider: MockProvider,
    /// GitHub App を有効にして起動した場合の wiremock。
    pub github: Option<MockGithub>,
    client: Client,
    /// drop されるとワーカーが停止する（`TestApp` の生存期間 = ワーカーの生存期間）。
    #[allow(dead_code)]
    worker_shutdown: watch::Sender<bool>,
}

impl TestApp {
    /// `GitlabSelfHostedProvider::new` は内部で `block_in_place` を使うため、
    /// 呼び出し側のテストは `#[tokio::test(flavor = "multi_thread")]` である必要がある。
    pub async fn new() -> Self {
        Self::new_with(TestAppOptions::default()).await
    }

    /// GitHub App を有効にしたアプリを立てる（API は wiremock を向く）。
    pub async fn new_with_github() -> Self {
        Self::new_with(TestAppOptions {
            github: Some(MockGithub::start().await),
            ..TestAppOptions::default()
        })
        .await
    }

    /// テスト専用ログイン口を有効にしたアプリを立てる（e2e の認証経路の検証用）。
    pub async fn new_with_test_login() -> Self {
        Self::new_with(TestAppOptions {
            test_login: true,
            ..TestAppOptions::default()
        })
        .await
    }

    pub async fn new_with(options: TestAppOptions) -> Self {
        init_tracing();
        ensure_test_env();

        let provider = MockProvider::start().await;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr: SocketAddr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");

        let mut settings = settings::load_settings().expect("load settings");
        // フロントのリダイレクト先検証（同一 origin）はテストアプリ自身を基準にする。
        settings.app_url = base_url.clone();
        settings.gitlab_instance_url = Some(provider.instance_url());
        settings.test_login_enabled = options.test_login;

        // GitHub App は「設定されていれば有効」。既定（`TestApp::new`）では未設定のまま
        // 起動して、連携が無効なときにフローが壊れないことも同時に確かめる。
        if let Some(github) = options.github.as_ref() {
            settings.github_app_id = Some(TEST_GITHUB_APP_ID);
            settings.github_app_private_key_pem = Some(TEST_GITHUB_APP_PRIVATE_KEY.to_string());
            settings.github_webhook_secret = Some(TEST_GITHUB_WEBHOOK_SECRET.to_string());
            settings.github_api_base_url = Some(github.uri());
        } else {
            settings.github_app_id = None;
            settings.github_app_private_key_pem = None;
            settings.github_webhook_secret = None;
            settings.github_api_base_url = None;
        }

        let db = common::db::connect_database(&settings.database_url)
            .await
            .expect("connect database");
        ensure_schema(&db).await;

        let redis_client = RedisConnection::new(&settings.redis_url);
        redis_client.ping().await.expect("redis ping");

        let pg_pool = setup_pool(&settings.database_url).await.expect("pg pool");
        // apalis-postgres のジョブテーブルもここで作られる。
        //
        // キューは TestApp ごとに分ける。1 プロセスで複数のテストが並行に走り、
        // それぞれが自分の tokio ランタイム上でワーカーを spawn するため、
        // キューを共有すると先に終わったテストのワーカーが他テストのジョブを
        // 掴んだまま消えてしまう。
        let compare_build_storage = setup_compare_build_storage_with_queue(
            &pg_pool,
            &format!("compare_build_test_{}", Uuid::new_v4().simple()),
        )
        .await
        .expect("compare build storage");
        let github_status_storage = setup_github_status_storage_with_queue(
            &pg_pool,
            &format!("github_status_test_{}", Uuid::new_v4().simple()),
        )
        .await
        .expect("github status storage");
        let github_webhook_storage = setup_github_webhook_storage_with_queue(
            &pg_pool,
            &format!("github_webhook_test_{}", Uuid::new_v4().simple()),
        )
        .await
        .expect("github webhook storage");
        let storage = service::storage::setup_storage()
            .await
            .expect("storage backend");
        let http_client = service::http::create_http_client().expect("http client");
        let oauth = Arc::new(
            service::oauth::OAuthRegistry::from_settings(
                &settings,
                redis_client.clone(),
                http_client.clone(),
            )
            .expect("oauth registry"),
        );

        let state = AppState {
            settings,
            db,
            pg_pool,
            redis_client,
            storage,
            oauth,
            compare_build_storage,
            github_status_storage,
            github_webhook_storage,
            http: http_client,
        };

        let session_store = SessionStore::<SessionRedisPool>::new(
            Some(state.redis_client.conn.clone().into()),
            test_session_config(),
        )
        .await
        .expect("session store");

        let (router, _) = routes::create_routes().split_for_parts();
        let router: Router = router
            .with_state(state.clone())
            .layer(middleware::from_fn_with_state(
                state.clone(),
                handler::middlewares::csrf::csrf_origin_check,
            ))
            .layer(SessionLayer::new(session_store));

        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve test app");
        });

        // 本番の server::run と同じ形でワーカーを走らせる。
        // これが無いと finalize したビルドが processing のまま止まる。
        let (worker_shutdown, worker_shutdown_rx) = watch::channel(false);
        backend::server::spawn_compare_build_worker(&state, worker_shutdown_rx.clone());
        backend::server::spawn_github_status_worker(&state, worker_shutdown_rx.clone());
        backend::server::spawn_github_webhook_worker(&state, worker_shutdown_rx);

        let client = Client::builder()
            .cookie_store(true)
            .redirect(Policy::none())
            .build()
            .expect("reqwest client");

        Self {
            state,
            base_url,
            provider,
            github: options.github,
            client,
            worker_shutdown,
        }
    }

    /// GitHub App 有効で立てたときの wiremock。
    pub fn github(&self) -> &MockGithub {
        self.github
            .as_ref()
            .expect("TestApp was not started with a github mock")
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// セッション Cookie を捨てた新しいクライアントに差し替える。
    pub fn reset_session_client(&mut self) {
        self.client = Client::builder()
            .cookie_store(true)
            .redirect(Policy::none())
            .build()
            .expect("reqwest client");
    }

    pub async fn get(&self, path: &str) -> Response {
        self.client
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .expect("get request")
    }

    pub async fn get_with_bearer(&self, path: &str, token: &str) -> Response {
        self.client
            .get(format!("{}{path}", self.base_url))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("bearer request")
    }

    pub async fn post_json(&self, path: &str, body: serde_json::Value) -> Response {
        self.client
            .post(format!("{}{path}", self.base_url))
            .json(&body)
            .send()
            .await
            .expect("post request")
    }

    pub async fn patch_json(&self, path: &str, body: serde_json::Value) -> Response {
        self.client
            .patch(format!("{}{path}", self.base_url))
            .json(&body)
            .send()
            .await
            .expect("patch request")
    }

    pub async fn post_json_with_bearer(
        &self,
        path: &str,
        token: &str,
        body: serde_json::Value,
    ) -> Response {
        self.client
            .post(format!("{}{path}", self.base_url))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .expect("bearer post request")
    }

    /// ボディ無しの POST（Bearer 付き）。finalize などに使う。
    pub async fn post_with_bearer(&self, path: &str, token: &str) -> Response {
        self.client
            .post(format!("{}{path}", self.base_url))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("bearer post request")
    }

    /// `name` + `file`（PNG）の multipart アップロード。
    /// フィールド順はハンドラの前提どおり `name` を先に送る。
    pub async fn upload_screenshot(
        &self,
        build_id: Uuid,
        token: &str,
        name: &str,
        png: Vec<u8>,
    ) -> Response {
        let part = reqwest::multipart::Part::bytes(png)
            .file_name(format!("{name}.png"))
            .mime_str("image/png")
            .expect("png mime");
        let form = reqwest::multipart::Form::new()
            .text("name", name.to_string())
            .part("file", part);

        self.client
            .post(format!(
                "{}/v1/ci/builds/{build_id}/screenshots",
                self.base_url
            ))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .expect("multipart upload")
    }

    /// GitHub からの webhook 配信を模した POST。
    ///
    /// `signature` が `None` なら [`TEST_GITHUB_WEBHOOK_SECRET`] で正しい署名を計算する。
    /// 署名不一致のテストは壊れた値を明示的に渡す。
    pub async fn post_github_webhook(
        &self,
        event: &str,
        payload: &serde_json::Value,
        signature: Option<&str>,
    ) -> Response {
        let body = serde_json::to_vec(payload).expect("serialize webhook payload");
        let signature = signature
            .map(str::to_owned)
            .unwrap_or_else(|| sign_webhook(TEST_GITHUB_WEBHOOK_SECRET, &body));

        self.client
            .post(format!("{}/v1/github/webhook", self.base_url))
            .header("X-GitHub-Event", event)
            .header("X-GitHub-Delivery", Uuid::new_v4().to_string())
            .header("X-Hub-Signature-256", signature)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .expect("github webhook request")
    }

    /// installation_id で `github_installations` を引く（webhook 処理の検証用）。
    pub async fn find_installation(
        &self,
        installation_id: i64,
    ) -> Option<entity::github_installations::Model> {
        entity::github_installations::Entity::find()
            .filter(entity::github_installations::Column::InstallationId.eq(installation_id))
            .one(&self.state.db)
            .await
            .expect("query github installation")
    }

    /// installation の行が現れるまで待つ（webhook はジョブ経由で非同期に処理される）。
    pub async fn wait_for_installation(
        &self,
        installation_id: i64,
        timeout: std::time::Duration,
    ) -> entity::github_installations::Model {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(model) = self.find_installation(installation_id).await {
                return model;
            }
            if std::time::Instant::now() >= deadline {
                panic!("github installation {installation_id} did not appear within {timeout:?}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// 条件を満たすまで installation を読み直す（`deleted_at` 等の反映待ち）。
    pub async fn wait_for_installation_where(
        &self,
        installation_id: i64,
        timeout: std::time::Duration,
        predicate: impl Fn(&entity::github_installations::Model) -> bool,
    ) -> entity::github_installations::Model {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(model) = self.find_installation(installation_id).await
                && predicate(&model)
            {
                return model;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "github installation {installation_id} never satisfied predicate within {timeout:?}"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub async fn delete(&self, path: &str) -> Response {
        self.client
            .delete(format!("{}{path}", self.base_url))
            .send()
            .await
            .expect("delete request")
    }

    // --- OAuth フローのヘルパー ---

    /// `/v1/auth/{provider}/login` を叩き、認可 URL へのリダイレクトを返す。
    pub async fn oauth_login(&self, redirect_to: Option<&str>) -> Response {
        let mut path = format!("/v1/auth/{TEST_PROVIDER}/login");
        if let Some(redirect_to) = redirect_to {
            path.push_str(&format!(
                "?redirect_to={}",
                urlencoding::encode(redirect_to)
            ));
        }
        self.get(&path).await
    }

    /// 認可 URL の `state` パラメータを取り出す。
    pub fn state_from_authorize_url(url: &str) -> String {
        url::Url::parse(url)
            .expect("authorize url")
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .expect("state query param")
    }

    /// プロバイダーからのコールバックを模して `/v1/auth/{provider}/callback` を叩く。
    pub async fn oauth_callback(&self, code: &str, state: &str) -> Response {
        self.get(&format!(
            "/v1/auth/{TEST_PROVIDER}/callback?code={}&state={}",
            urlencoding::encode(code),
            urlencoding::encode(state)
        ))
        .await
    }

    /// login → callback まで通してログインする。コールバックのレスポンスを返す。
    pub async fn login_via_oauth(&self) -> Response {
        let start = self.oauth_login(None).await;
        assert!(is_redirect(start.status()), "oauth login should redirect");
        let state = Self::state_from_authorize_url(&location_of(&start));
        self.oauth_callback("mock-auth-code", &state).await
    }

    /// ログインして、そのセッションのユーザーレコードを返す。
    ///
    /// `TestApp` は 1 インスタンスにつき 1 人の OAuth ユーザーを持つため、
    /// 複数ユーザーが必要なテスト（ロールマトリクス等）は `TestApp` を複数作る。
    /// DB / Valkey はプロセス内で共有される。
    pub async fn login_as_new_user(&self) -> users::Model {
        assert!(
            is_redirect(self.login_via_oauth().await.status()),
            "oauth login should redirect"
        );
        self.find_user_by_username(&self.provider.username)
            .await
            .expect("logged in oauth user")
    }

    // --- DB ヘルパー ---

    pub async fn find_user_by_username(&self, username: &str) -> Option<users::Model> {
        users::Entity::find()
            .filter(users::Column::Username.eq(username))
            .one(&self.state.db)
            .await
            .expect("query user")
    }

    /// このテストの OAuth ユーザー名を接頭辞に持つユーザー数。
    /// DB は並列テストで共有されるため、全件カウントは使えない。
    pub async fn count_own_users(&self) -> usize {
        users::Entity::find()
            .filter(users::Column::Username.starts_with(&self.provider.username))
            .all(&self.state.db)
            .await
            .expect("query users")
            .len()
    }

    pub async fn connections_for_user(&self, user_id: Uuid) -> Vec<oauth_connections::Model> {
        oauth_connections::Entity::find()
            .filter(oauth_connections::Column::UserId.eq(user_id))
            .all(&self.state.db)
            .await
            .expect("query connections")
    }

    pub async fn personal_token(&self, token_id: Uuid) -> personal_tokens::Model {
        personal_tokens::Entity::find_by_id(token_id)
            .one(&self.state.db)
            .await
            .expect("query personal token")
            .expect("personal token exists")
    }

    /// テスト用にユーザーを直接作る（OAuth を経由しない）。
    pub async fn insert_user(&self) -> users::Model {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().fixed_offset();
        users::ActiveModel {
            id: Set(id),
            username: Set(format!("test_{}", &id.to_string()[..8])),
            display_name: Set("Test User".into()),
            avatar_url: Set(None),
            email: Set(Some(format!("test-{id}@example.com"))),
            sessions_revoked_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&self.state.db)
        .await
        .expect("insert user")
    }

    /// PAT を DB に直接発行する（セッションを経由しないケース用）。
    pub async fn insert_personal_token(&self, user_id: Uuid, scopes: Vec<Scope>) -> (String, Uuid) {
        let (token, token_hash) =
            service::auth::generate_personal_token(&self.state.settings.personal_token_secret)
                .expect("generate pat");
        let id = Uuid::new_v4();
        personal_tokens::ActiveModel {
            id: Set(id),
            name: Set("integration-test".into()),
            token_last_four: Set(service::auth::token_last_four(&token)),
            token_hash: Set(token_hash),
            expires_at: Set(None),
            last_used_at: Set(None),
            revoked: Set(false),
            user_id: Set(user_id),
            scopes: Set(entity::scopes::ScopeList(scopes)),
            created_at: Set(chrono::Utc::now().fixed_offset()),
        }
        .insert(&self.state.db)
        .await
        .expect("insert pat");
        (token, id)
    }
}
