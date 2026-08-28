//! Postgres の compare_build / github_status / github_webhook キューを消費する
//! 独立ワーカー。
//!
//! HTTP は提供しない。API プロセスへ同居させると、ワーカーが止まったときの
//! 復帰（プロセスを落として restart policy に任せる）が HTTP の可用性を巻き
//! 込む。`vrt-runner` と同じ形で切り出し、再起動の影響をキューの中に閉じる。
//!
//! 渡す設定も API より狭い。PAT の署名鍵や OAuth の資格情報はどのワーカーも
//! 読まないので受け取らない。

use std::env;

use backend::server::{
    spawn_compare_build_worker_with_state, spawn_github_status_worker_with_state,
    spawn_github_webhook_worker_with_state,
};
use backend::supervision::{SupervisedTask, run_until_shutdown};
use common::settings::{DEFAULT_GITHUB_API_BASE_URL, JobSettings};
use job::JobState;
use tokio::sync::watch;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn required_env(name: &str) -> Result<String, std::io::Error> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| std::io::Error::other(format!("{name} is required for vrt-worker")))
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// ワーカーが読む設定だけを環境変数から組む。
fn job_settings() -> Result<JobSettings, std::io::Error> {
    let github_api_base_url = optional_env("GITHUB_API_BASE_URL")
        .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string();

    // API と同じく、PEM は改行を \n にエスケープして 1 行で渡せるようにする。
    let github_app_private_key_pem =
        optional_env("GITHUB_APP_PRIVATE_KEY_PEM").map(|pem| pem.replace("\\n", "\n"));

    let github_app_id = match optional_env("GITHUB_APP_ID") {
        Some(raw) => Some(raw.parse::<u64>().map_err(|_| {
            std::io::Error::other(format!("GITHUB_APP_ID must be a number, got `{raw}`"))
        })?),
        None => None,
    };

    Ok(JobSettings {
        app_url: required_env("APP_URL")?,
        github_api_base_url,
        github_app_id,
        github_app_private_key_pem,
        storage_min_retention_days: optional_env("STORAGE_MIN_RETENTION_DAYS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    })
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
    let redis_url = required_env("REDIS_URL")?;
    let settings = job_settings()?;

    let db = common::db::connect_database(&database_url).await?;
    let redis_client = common::cache::redis::RedisConnection::new(&redis_url);
    let pg_pool = backend::jobs::setup_pool(&database_url).await?;
    let compare_build_storage = backend::jobs::setup_compare_build_storage(&pg_pool).await?;
    let github_status_storage = backend::jobs::setup_github_status_storage(&pg_pool).await?;
    let github_webhook_storage = backend::jobs::setup_github_webhook_storage(&pg_pool).await?;
    let storage = backend::utils::storage::setup_storage()
        .await
        .map_err(|error| {
            std::io::Error::other(format!("worker storage initialization failed: {error}"))
        })?;

    if settings.github_app_id.is_none() || settings.github_app_private_key_pem.is_none() {
        // 起動は止めない（API と同じ扱い）。ステータスと PR コメントだけが無効になる。
        tracing::warn!(
            "GITHUB_APP_ID / GITHUB_APP_PRIVATE_KEY_PEM is not set; commit statuses and PR comments will be skipped"
        );
    }

    let job_state = JobState {
        settings,
        db,
        redis_client,
        storage,
        http: service::http::create_http_client()?,
        github_status_storage: github_status_storage.clone(),
        compare_build_storage: compare_build_storage.clone(),
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let workers = vec![
        (
            "compare build worker",
            spawn_compare_build_worker_with_state(
                compare_build_storage,
                job_state.clone(),
                shutdown_rx.clone(),
            ),
        ),
        (
            "github status worker",
            spawn_github_status_worker_with_state(
                github_status_storage,
                job_state.clone(),
                shutdown_rx.clone(),
            ),
        ),
        (
            "github webhook worker",
            spawn_github_webhook_worker_with_state(
                github_webhook_storage,
                job_state,
                shutdown_rx.clone(),
            ),
        ),
    ];

    // ハートビート監視は自分が登録したワーカー ID だけを見る。
    let watched: Vec<job::liveness::WatchedWorker> = workers
        .iter()
        .map(|(_, worker)| job::liveness::WatchedWorker {
            queue: worker.queue.clone(),
            worker_id: worker.worker_id.clone(),
        })
        .collect();

    let mut tasks: Vec<SupervisedTask> = workers
        .into_iter()
        .map(|(label, worker)| SupervisedTask::new(label, worker.handle))
        .collect();

    let monitor_shutdown = shutdown_rx.clone();
    tasks.push(SupervisedTask::new(
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
    ));

    info!("vrt-worker ready; polling compare_build / github_status / github_webhook queues");
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
