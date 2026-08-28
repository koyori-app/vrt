//! Postgres の `render_build` キューだけを消費する独立 Storybook runner。
//!
//! HTTP API、Redis、OAuth/GitHub の資格情報は不要。API と同じ DATABASE_URL と
//! STORAGE_BACKEND 設定、レンダリング用 CHROMIUM_PATH だけを受け取る。

use std::env;

use backend::server::spawn_render_build_worker_with_state;
use backend::supervision::{SupervisedTask, run_until_shutdown};
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
    let worker =
        spawn_render_build_worker_with_state(render_build_storage, state, shutdown_rx.clone());

    // ハートビートが止まるだけの経路にも備える（ワーカー ID は自分の分だけ見る）。
    let watched = vec![job::liveness::WatchedWorker {
        queue: worker.queue.clone(),
        worker_id: worker.worker_id.clone(),
    }];
    let monitor_shutdown = shutdown_rx.clone();
    let tasks = vec![
        SupervisedTask::new("render build worker", worker.handle),
        SupervisedTask::new(
            "worker heartbeat monitor",
            tokio::spawn(async move {
                job::liveness::watch_heartbeats(
                    pg_pool,
                    watched,
                    job::liveness::LivenessConfig::from_env(),
                    monitor_shutdown,
                )
                .await
            }),
        ),
    ];

    info!(%chromium_path, "vrt-runner ready; polling render_build queue");
    run_until_shutdown(tasks, shutdown_tx, shutdown_signal()).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ワーカーが停止要求より先に終わったら、runner はプロセスごと落ちる。
    /// PID だけ残すと restart policy が発火せず、キューが永久に止まる。
    #[tokio::test]
    async fn worker_exit_before_shutdown_is_an_error() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let tasks = vec![SupervisedTask::new(
            "render build worker",
            tokio::spawn(async { Ok::<(), std::io::Error>(()) }),
        )];

        let error = run_until_shutdown(tasks, shutdown_tx, std::future::pending())
            .await
            .expect_err("a runner must not outlive its worker");

        assert!(
            error.to_string().contains("stopped unexpectedly"),
            "{error}"
        );
    }

    /// 停止要求が先なら、在庫を捌き終えてから 0 で終わる。
    #[tokio::test]
    async fn shutdown_signal_stops_the_worker_gracefully() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let tasks = vec![SupervisedTask::new(
            "render build worker",
            tokio::spawn(async move {
                shutdown_rx.changed().await.expect("shutdown sender");
                assert!(*shutdown_rx.borrow());
                Ok::<(), std::io::Error>(())
            }),
        )];

        run_until_shutdown(tasks, shutdown_tx, std::future::ready(()))
            .await
            .expect("signal-driven shutdown is successful");
    }
}
