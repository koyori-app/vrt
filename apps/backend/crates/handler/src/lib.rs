//! HTTP ハンドラー層: axum ハンドラー・ルーティング・ミドルウェア。

use std::sync::Arc;

use apalis_postgres::PgPool;
use common::cache::redis::RedisConnection;
use common::settings::Settings;
use job::{CompareBuildStorage, GithubStatusStorage, GithubWebhookStorage};
use sea_orm::DatabaseConnection;
use service::oauth::OAuthRegistry;
use service::storage::StorageBackend;

// 旧 crate::error / crate::settings パス互換のための再公開。
pub use common::{error, settings};

pub mod extractors;
pub mod handlers;
pub mod middlewares;
pub mod openapi;
pub mod routes;

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub db: DatabaseConnection,
    pub pg_pool: PgPool,
    pub redis_client: RedisConnection,
    pub storage: Arc<dyn StorageBackend>,
    /// 設定済み OAuth プロバイダー + Redis の state ストア + 共有 HTTP クライアント。
    pub oauth: Arc<OAuthRegistry>,
    /// `CompareBuildJob` の apalis ストレージ（finalize ハンドラが push する）。
    pub compare_build_storage: Arc<CompareBuildStorage>,
    /// `GithubStatusJob` の apalis ストレージ（finalize / approve / reject が push する）。
    pub github_status_storage: Arc<GithubStatusStorage>,
    /// `GithubWebhookJob` の apalis ストレージ（webhook ハンドラが push する）。
    pub github_webhook_storage: Arc<GithubWebhookStorage>,
    /// 外部 API 呼び出し用の共有 HTTP クライアント（GitHub API）。
    pub http: reqwest::Client,
}
