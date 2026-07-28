//! Apalis バックグラウンドジョブ

pub mod compare_build;
pub mod github_status;
pub mod github_webhook;
pub mod render_build;

use std::sync::Arc;

use apalis_postgres::PgPool;

pub use compare_build::{CompareBuildJob, CompareBuildStorage};
pub use github_status::{GithubStatusJob, GithubStatusStorage};
pub use github_webhook::{GithubWebhookJob, GithubWebhookStorage};
pub use render_build::{RenderBuildJob, RenderBuildStorage};
use sea_orm::DatabaseConnection;

use common::settings::Settings;
use service::storage::StorageBackend;

/// ワーカーが必要とする依存の束。
/// AppState（handler クレート）を受け取ると job → handler の循環になるため、
/// ワーカーは実際に使う要素だけをここから受け取る。
#[derive(Clone)]
pub struct JobState {
    pub settings: Arc<Settings>,
    pub db: DatabaseConnection,
    pub redis_client: common::cache::redis::RedisConnection,
    pub storage: Arc<dyn StorageBackend>,
    /// 外部 API 呼び出し用の共有 HTTP クライアント（GitHub API）。
    pub http: reqwest::Client,
    /// `compare_build` の完了時に `GithubStatusJob` を投入するために持つ。
    pub github_status_storage: Arc<GithubStatusStorage>,
    /// `render_build` の完了時に `CompareBuildJob` を投入するために持つ。
    pub compare_build_storage: Arc<CompareBuildStorage>,
}

/// apalis 用 Postgres プールの既定上限。
///
/// ワーカーが実際に使うのは「フェッチ + ack + keep_alive + 孤児再投入」程度で、
/// 同時に何本も要らない。既定(10)のままだと 1 プロセスで複数のアプリを立てる
/// 統合テストで Postgres の `max_connections` を食い潰す。
pub const DEFAULT_APALIS_MAX_CONNECTIONS: u32 = 5;

/// 環境変数 `APALIS_MAX_CONNECTIONS` から上限を読む（未設定/不正なら既定値）。
pub fn apalis_max_connections() -> u32 {
    std::env::var("APALIS_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_APALIS_MAX_CONNECTIONS)
}

/// apalis のジョブテーブル（`apalis` スキーマ）作成をプロセス内で 1 回に絞るためのゲート。
///
/// `PostgresStorage::setup` はキューごとではなく **DB 全体に対する** マイグレーションで、
/// 何度呼んでも結果は同じ。だがキューの数だけ（VRT では 3 本）呼ぶと、
/// 1 プロセスで多数のアプリを立てる統合テストでは
/// 「アプリ数 × キュー数」回のマイグレーションが同時に走って Postgres を詰まらせ、
/// 別のテストの `connect_database` が `PoolTimedOut` する。
///
/// プロセス内の全プールは同じ `DATABASE_URL` を指す前提なので、最初の 1 回だけ実行する。
static APALIS_SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// apalis のジョブテーブルを（プロセス内で 1 回だけ）作成する。
pub async fn ensure_apalis_schema(pool: &PgPool) -> Result<(), anyhow::Error> {
    APALIS_SCHEMA_READY
        .get_or_try_init(|| async {
            // `setup` は型引数に依存しない DB 全体のマイグレーション
            // （apalis 側でも `PostgresStorage<(), (), ()>` に生えている）。
            apalis_postgres::PostgresStorage::<(), (), ()>::setup(pool)
                .await
                .map_err(|e| anyhow::anyhow!("setup apalis schema: {e}"))
        })
        .await?;
    Ok(())
}

/// 上限を明示した apalis 用プールを作る。
pub async fn setup_pool(database_url: &str) -> Result<PgPool, anyhow::Error> {
    Ok(sqlx::postgres::PgPoolOptions::new()
        .max_connections(apalis_max_connections())
        .min_connections(0)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(database_url)
        .await?)
}

/// `CompareBuildJob` のストレージ（apalis-postgres のマイグレーションもここで走る）。
pub async fn setup_compare_build_storage(
    pool: &PgPool,
) -> Result<Arc<CompareBuildStorage>, anyhow::Error> {
    compare_build::setup(pool).await
}

/// キュー名を指定して `CompareBuildJob` のストレージを作る（統合テスト用）。
pub async fn setup_compare_build_storage_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<CompareBuildStorage>, anyhow::Error> {
    compare_build::setup_with_queue(pool, queue).await
}

/// `GithubStatusJob` のストレージ。
pub async fn setup_github_status_storage(
    pool: &PgPool,
) -> Result<Arc<GithubStatusStorage>, anyhow::Error> {
    github_status::setup(pool).await
}

/// キュー名を指定して `GithubStatusJob` のストレージを作る（統合テスト用）。
pub async fn setup_github_status_storage_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<GithubStatusStorage>, anyhow::Error> {
    github_status::setup_with_queue(pool, queue).await
}

/// `GithubWebhookJob` のストレージ。
pub async fn setup_github_webhook_storage(
    pool: &PgPool,
) -> Result<Arc<GithubWebhookStorage>, anyhow::Error> {
    github_webhook::setup(pool).await
}

/// キュー名を指定して `GithubWebhookJob` のストレージを作る（統合テスト用）。
pub async fn setup_github_webhook_storage_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<GithubWebhookStorage>, anyhow::Error> {
    github_webhook::setup_with_queue(pool, queue).await
}

/// `RenderBuildJob` のストレージ。
pub async fn setup_render_build_storage(
    pool: &PgPool,
) -> Result<Arc<RenderBuildStorage>, anyhow::Error> {
    render_build::setup(pool).await
}

/// キュー名を指定して `RenderBuildJob` のストレージを作る（統合テスト用）。
pub async fn setup_render_build_storage_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<RenderBuildStorage>, anyhow::Error> {
    render_build::setup_with_queue(pool, queue).await
}
