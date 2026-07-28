use backend::{AppState, server::run};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings = backend::settings::load_settings()?;

    let db = common::db::connect_database(&settings.database_url).await?;
    db.get_schema_registry("entity::*").sync(&db).await?;

    let redis_client = common::cache::redis::RedisConnection::new(&settings.redis_url);
    redis_client.ping().await?;

    let pg_pool = backend::jobs::setup_pool(&settings.database_url).await?;
    // apalis-postgres のジョブテーブルのマイグレーションもここで走る。
    let compare_build_storage = backend::jobs::setup_compare_build_storage(&pg_pool).await?;
    let github_status_storage = backend::jobs::setup_github_status_storage(&pg_pool).await?;
    let github_webhook_storage = backend::jobs::setup_github_webhook_storage(&pg_pool).await?;
    let render_build_storage = backend::jobs::setup_render_build_storage(&pg_pool).await?;

    let storage = backend::utils::storage::setup_storage().await.map_err(|e| {
        std::io::Error::other(format!(
            "storage backend initialization failed (STORAGE_BACKEND / S3_* / LOCAL_UPLOAD_DIR): {e}"
        ))
    })?;

    let http_client = service::http::create_http_client()?;
    let oauth = std::sync::Arc::new(service::oauth::OAuthRegistry::from_settings(
        &settings,
        redis_client.clone(),
        http_client.clone(),
    )?);

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
        render_build_storage,
        http: http_client,
    };
    run(state).await?;

    Ok(())
}
