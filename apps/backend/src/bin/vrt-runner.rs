//! Postgres の `render_build` キューだけを消費する独立 Storybook runner。
//!
//! HTTP API、Redis、OAuth/GitHub の資格情報は不要。API と同じ DATABASE_URL と
//! STORAGE_BACKEND 設定、レンダリング用 CHROMIUM_PATH だけを受け取る。

use std::{env, future::Future};

use backend::server::spawn_render_build_worker_with_state;
use job::RenderJobState;
use tokio::sync::watch;
use tracing::info;
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

    // API と同じ STORAGE_MIN_RETENTION_DAYS を渡す（未設定・不正値は 0 = 無効）。
    let storage_min_retention_days = env::var("STORAGE_MIN_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);

    let state = RenderJobState {
        chromium_path: chromium_path.clone(),
        storage_min_retention_days,
        db,
        storage,
        github_status_storage,
        compare_build_storage,
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = spawn_render_build_worker_with_state(render_build_storage, state, shutdown_rx);

    info!(%chromium_path, "vrt-runner ready; polling render_build queue");
    supervise_worker(handle, shutdown_tx, shutdown_signal()).await?;
    Ok(())
}

/// OS の停止通知と worker 自身の終了を同時に監視する。
///
/// worker が先に止まった場合に PID だけ残すと、Compose / Dokploy の restart policy が
/// 発火せずキューが永久に止まる。正常終了に見えても runner にとっては異常なので、
/// エラーを返してプロセスを非ゼロ終了させる。
async fn supervise_worker<E, F>(
    mut handle: tokio::task::JoinHandle<Result<(), E>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown: F,
) -> Result<(), std::io::Error>
where
    E: std::fmt::Display,
    F: Future<Output = ()>,
{
    tokio::pin!(shutdown);

    tokio::select! {
        result = &mut handle => match result {
            Ok(Ok(())) => Err(std::io::Error::other("render worker stopped unexpectedly")),
            Ok(Err(error)) => Err(std::io::Error::other(format!(
                "render worker stopped with an error: {error}"
            ))),
            Err(error) => Err(std::io::Error::other(format!(
                "render worker task failed: {error}"
            ))),
        },
        () = &mut shutdown => {
            let _ = shutdown_tx.send(true);
            info!("vrt-runner shutting down; waiting for in-flight build");
            match handle.await {
                Ok(Ok(())) => {
                    info!("render worker stopped");
                    Ok(())
                }
                Ok(Err(error)) => Err(std::io::Error::other(format!(
                    "render worker stopped with an error during shutdown: {error}"
                ))),
                Err(error) => Err(std::io::Error::other(format!(
                    "render worker task failed during shutdown: {error}"
                ))),
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn worker_exit_before_shutdown_is_an_error() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async { Ok::<(), std::io::Error>(()) });

        let error = supervise_worker(handle, shutdown_tx, std::future::pending())
            .await
            .expect_err("a runner must not outlive its worker");

        assert!(error.to_string().contains("stopped unexpectedly"));
    }

    #[tokio::test]
    async fn shutdown_signal_stops_the_worker_gracefully() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            shutdown_rx.changed().await.expect("shutdown sender");
            assert!(*shutdown_rx.borrow());
            Ok::<(), std::io::Error>(())
        });

        supervise_worker(handle, shutdown_tx, std::future::ready(()))
            .await
            .expect("signal-driven shutdown is successful");
    }
}
