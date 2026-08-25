//! Postgres の `render_build` キューだけを消費する独立 Storybook runner。
//!
//! HTTP API、Redis、OAuth/GitHub の資格情報は不要。API と同じ DATABASE_URL と
//! STORAGE_BACKEND 設定、レンダリング用 CHROMIUM_PATH だけを受け取る。

use std::env;

use backend::server::spawn_render_build_worker_with_state;
use job::RenderJobState;
use tokio::sync::watch;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn required_env(name: &str) -> Result<String, std::io::Error> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| std::io::Error::other(format!("{name} is required for vrt-runner")))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            env::var("RUST_LOG").unwrap_or_else(|_| "info,sqlx=warn".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = required_env("DATABASE_URL")?;
    let chromium_path = required_env("CHROMIUM_PATH")?;

    let db = common::db::connect_database(&database_url).await?;
    let pg_pool = backend::jobs::setup_pool(&database_url).await?;
    let compare_build_storage = backend::jobs::setup_compare_build_storage(&pg_pool).await?;
    let github_status_storage = backend::jobs::setup_github_status_storage(&pg_pool).await?;
    let render_build_storage = backend::jobs::setup_render_build_storage(&pg_pool).await?;
    let storage = backend::utils::storage::setup_storage()
        .await
        .map_err(|error| {
            std::io::Error::other(format!("runner storage initialization failed: {error}"))
        })?;

    let state = RenderJobState {
        chromium_path: chromium_path.clone(),
        db,
        storage,
        github_status_storage,
        compare_build_storage,
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = spawn_render_build_worker_with_state(render_build_storage, state, shutdown_rx);

    info!(%chromium_path, "vrt-runner ready; polling render_build queue");
    shutdown_signal().await;
    let _ = shutdown_tx.send(true);
    info!("vrt-runner shutting down; waiting for in-flight build");

    match handle.await {
        Ok(Ok(())) => info!("render worker stopped"),
        Ok(Err(error)) => warn!(%error, "render worker stopped with an error"),
        Err(error) => warn!(%error, "render worker task failed"),
    }

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
