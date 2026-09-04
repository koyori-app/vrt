use std::sync::Arc;

use apalis::layers::WorkerBuilderExt;
use apalis::layers::retry::RetryPolicy;
use apalis::prelude::WorkerBuilder;
use axum::{Router, http::HeaderValue, http::Method, middleware};
use axum_session::{SameSite, SessionConfig, SessionLayer, SessionStore};
use axum_session_redispool::SessionRedisPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tower_http::cors::{AllowHeaders, CorsLayer};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa_scalar::{Scalar, Servable};

use crate::supervision::{DRAIN_DEADLINE, SupervisedTask, TaskWatcher};
use handler::AppState;
use handler::middlewares::logging::logging_middleware;
use job::{JobState, RenderBuildStorage, RenderJobState};
use job::{compare_build, github_status, github_webhook, render_build};

/// `AppState` からワーカー用の依存だけを取り出す。
///
/// `JobState` に `AppState` をそのまま渡すと job → handler の循環依存になるため、
/// 必要な要素（DB・Redis・ストレージ・設定）だけを移し替える。
pub fn job_state_from(state: &AppState) -> JobState {
    JobState {
        settings: (&state.settings).into(),
        db: state.db.clone(),
        redis_client: state.redis_client.clone(),
        storage: state.storage.clone(),
        http: state.http.clone(),
        github_status_storage: state.github_status_storage.clone(),
        compare_build_storage: state.compare_build_storage.clone(),
    }
}

/// `AppState` から Render worker 専用の依存だけを取り出す。
///
/// Chromium が無い API プロセスでは呼ばない。独立 `vrt-runner` は HTTP API の
/// state を組み立てず、同じ [`RenderJobState`] を直接生成する。
pub fn render_job_state_from(state: &AppState) -> Option<RenderJobState> {
    let chromium_path = state
        .settings
        .chromium_path
        .clone()
        .filter(|path| !path.trim().is_empty())?;

    Some(RenderJobState {
        chromium_path,
        storage_min_retention_days: state.settings.storage_min_retention_days,
        db: state.db.clone(),
        storage: state.storage.clone(),
        github_status_storage: state.github_status_storage.clone(),
        compare_build_storage: state.compare_build_storage.clone(),
    })
}

/// ワーカー名（= `apalis.workers.id`）を組み立てる。
///
/// **必ずワーカーインスタンスごとに一意にすること。**
/// apalis の `queries/worker/register.sql` は
///
/// ```sql
/// INSERT INTO apalis.workers (id, ...) VALUES (...)
/// ON CONFLICT (id) DO UPDATE SET ...
/// WHERE pg_try_advisory_lock(hashtext(workers.id));
/// ```
///
/// で登録する。`apalis.workers.id` には UNIQUE インデックスがあり、
/// 更新はセッションレベルのアドバイザリロックを取れたワーカーだけが通る。
/// つまり**同じ名前のワーカーは「1 つだけ生き残り、残りは登録に失敗して停止する」**
/// （apalis 側の意図的な単一ワーカー保証）。
///
/// 名前を固定値にすると:
/// - 本番: バックエンドを複数レプリカに増やしても 1 台しかジョブを捌かない（水平スケールが無言で効かない）
/// - テスト: 1 プロセスで複数の TestApp を立てると最初の 1 つ以外のワーカーが死に、
///   そのアプリのジョブが `Pending` のまま永久に残る
///
/// キュー名 + プロセスごとに一意な UUID で構成する。
/// 起動したワーカー 1 本。
///
/// ハートビート監視は `apalis.workers.id` で自分の行だけを引くので、
/// 生成した名前を呼び出し元へ返す必要がある。
pub struct SpawnedWorker {
    pub queue: String,
    /// `apalis.workers.id`。プロセスごとに一意。
    pub worker_id: String,
    pub handle: JoinHandle<Result<(), apalis::prelude::WorkerError>>,
}

impl SpawnedWorker {
    fn watched(&self) -> job::liveness::WatchedWorker {
        job::liveness::WatchedWorker {
            queue: self.queue.clone(),
            worker_id: self.worker_id.clone(),
        }
    }
}

fn worker_name(queue: &str) -> String {
    format!("{queue}-worker-{}", uuid::Uuid::new_v4().simple())
}

/// `CompareBuildJob` のワーカーを spawn する。
///
/// 本番（[`run`]）とテストハーネスの両方から使う。`shutdown` に `true` が送られると
/// 実行中のジョブを完了させてから停止する。
pub fn spawn_compare_build_worker(
    state: &AppState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    spawn_compare_build_worker_with_state(
        state.compare_build_storage.clone(),
        job_state_from(state),
        shutdown,
    )
}

/// HTTP API から独立した `vrt-worker` も使う共通起動口。
pub fn spawn_compare_build_worker_with_state(
    storage: Arc<job::CompareBuildStorage>,
    job_state: JobState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    let storage = storage.as_ref().clone();

    // キューはストレージの設定から取る（テストはキューを差し替えるため定数を使わない）。
    let queue = storage.config().queue().to_string();
    let worker_id = worker_name(&queue);
    info!(%queue, worker = %worker_id, "starting compare_build worker");

    let worker = WorkerBuilder::new(worker_id.clone())
        .backend(storage)
        .retry(RetryPolicy::retries(compare_build::MAX_RETRIES))
        .enable_tracing()
        .concurrency(compare_build::WORKER_CONCURRENCY)
        .data(job_state)
        .build(compare_build::process);

    SpawnedWorker {
        queue,
        worker_id: worker_id.clone(),
        handle: tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await }),
    }
}

/// `GithubStatusJob` のワーカーを spawn する（ワーカー名の一意性は [`worker_name`] 参照）。
pub fn spawn_github_status_worker(
    state: &AppState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    spawn_github_status_worker_with_state(
        state.github_status_storage.clone(),
        job_state_from(state),
        shutdown,
    )
}

/// HTTP API から独立した `vrt-worker` も使う共通起動口。
pub fn spawn_github_status_worker_with_state(
    storage: Arc<job::GithubStatusStorage>,
    job_state: JobState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    let storage = storage.as_ref().clone();

    let queue = storage.config().queue().to_string();
    let worker_id = worker_name(&queue);
    info!(%queue, worker = %worker_id, "starting github_status worker");

    let worker = WorkerBuilder::new(worker_id.clone())
        .backend(storage)
        .retry(RetryPolicy::retries(github_status::MAX_RETRIES))
        .enable_tracing()
        .concurrency(github_status::WORKER_CONCURRENCY)
        .data(job_state)
        .build(github_status::process);

    SpawnedWorker {
        queue,
        worker_id: worker_id.clone(),
        handle: tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await }),
    }
}

/// `RenderBuildJob` のワーカーを spawn する（ワーカー名の一意性は [`worker_name`] 参照）。
///
/// Chromium を起動するため 1 ジョブが重い。ジョブの同時実行数は
/// [`render_build::WORKER_CONCURRENCY`]（= 1）に絞り、ジョブ内の story だけを
/// [`render_build::STORY_RENDER_CONCURRENCY`]（= 2）で並列化する。
pub fn spawn_render_build_worker(
    state: &AppState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    let job_state = render_job_state_from(state)
        .expect("spawn_render_build_worker requires a configured CHROMIUM_PATH");
    spawn_render_build_worker_with_state(state.render_build_storage.clone(), job_state, shutdown)
}

/// HTTP API から独立した runner も利用できる Render worker の共通起動口。
pub fn spawn_render_build_worker_with_state(
    storage: Arc<RenderBuildStorage>,
    job_state: RenderJobState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    let storage = storage.as_ref().clone();

    let queue = storage.config().queue().to_string();
    let worker_id = worker_name(&queue);
    info!(%queue, worker = %worker_id, "starting render_build worker");

    let worker = WorkerBuilder::new(worker_id.clone())
        .backend(storage)
        .retry(RetryPolicy::retries(render_build::MAX_RETRIES))
        .enable_tracing()
        .concurrency(render_build::WORKER_CONCURRENCY)
        .data(job_state)
        .build(render_build::process);

    SpawnedWorker {
        queue,
        worker_id: worker_id.clone(),
        handle: tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await }),
    }
}

/// `GithubWebhookJob` のワーカーを spawn する。
pub fn spawn_github_webhook_worker(
    state: &AppState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    spawn_github_webhook_worker_with_state(
        state.github_webhook_storage.clone(),
        job_state_from(state),
        shutdown,
    )
}

/// HTTP API から独立した `vrt-worker` も使う共通起動口。
pub fn spawn_github_webhook_worker_with_state(
    storage: Arc<job::GithubWebhookStorage>,
    job_state: JobState,
    shutdown: watch::Receiver<bool>,
) -> SpawnedWorker {
    let storage = storage.as_ref().clone();

    let queue = storage.config().queue().to_string();
    let worker_id = worker_name(&queue);
    info!(%queue, worker = %worker_id, "starting github_webhook worker");

    let worker = WorkerBuilder::new(worker_id.clone())
        .backend(storage)
        .retry(RetryPolicy::retries(github_webhook::MAX_RETRIES))
        .enable_tracing()
        .concurrency(github_webhook::WORKER_CONCURRENCY)
        .data(job_state)
        .build(github_webhook::process);

    SpawnedWorker {
        queue,
        worker_id: worker_id.clone(),
        handle: tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await }),
    }
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) -> Result<(), std::io::Error> {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
    Ok(())
}

/// worker の終了が停止要求に伴うものかを判定する。
///
/// `watcher.first_exit()` と shutdown 通知は同時に ready になり得るため、
/// `select!` の選択結果だけで異常終了を判定してはいけない。worker は停止要求を
/// 観測してから正常終了するので、終了通知を受けた直後にフラグを再確認すれば、
/// graceful shutdown との競合を安全に正常扱いできる。
fn is_unexpected_worker_exit(shutdown: &watch::Receiver<bool>) -> bool {
    !*shutdown.borrow()
}

pub async fn run(state: AppState) -> Result<(), Box<dyn std::error::Error>> {
    let log_filter = tracing_subscriber::EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info,sqlx=warn".into()),
    );

    tracing_subscriber::registry()
        .with(log_filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let is_prod = std::env::var("RUST_ENV").unwrap_or_default() == "production";
    let settings = &state.settings;
    let addr = settings.listen_addr.clone();

    let session_config = SessionConfig::default()
        .with_secure(is_prod)
        .with_cookie_same_site(if is_prod {
            SameSite::None
        } else {
            SameSite::Lax
        });

    let session_store = SessionStore::<SessionRedisPool>::new(
        Some(state.redis_client.conn.clone().into()),
        session_config,
    )
    .await?;

    let (router, mut openapi) = utoipa_axum::router::OpenApiRouter::new()
        .merge(handler::routes::create_routes())
        .split_for_parts();

    handler::openapi::register_schemas(&mut openapi);

    let cors = CorsLayer::new()
        .allow_origin(settings.allow_origin.parse::<HeaderValue>()?)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers(AllowHeaders::mirror_request())
        .allow_credentials(true);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut workers: Vec<(&str, SpawnedWorker)> = Vec::new();

    if state.settings.job_workers_enabled {
        workers.push((
            "compare build worker",
            spawn_compare_build_worker(&state, shutdown_rx.clone()),
        ));
        workers.push((
            "github status worker",
            spawn_github_status_worker(&state, shutdown_rx.clone()),
        ));
        workers.push((
            "github webhook worker",
            spawn_github_webhook_worker(&state, shutdown_rx.clone()),
        ));
    } else {
        info!("API job workers disabled; waiting for an external vrt-worker instance");
    }

    if state.settings.render_worker_enabled && state.settings.storybook_render_enabled() {
        if state.settings.chromium_configured() {
            workers.push((
                "render build worker",
                spawn_render_build_worker(&state, shutdown_rx.clone()),
            ));
            info!(
                chromium = %state.settings.chromium_path.clone().unwrap_or_default(),
                "storybook rendering enabled in API process"
            );
        } else {
            warn!("render worker enabled without CHROMIUM_PATH; render worker was not started");
        }
    } else if !state.settings.render_worker_enabled && state.settings.storybook_render_enabled() {
        info!("API render worker disabled; waiting for external vrt-runner instances");
    }

    if !state.settings.storybook_render_enabled() {
        warn!("Storybook rendering disabled; storybook-mode builds will be rejected at creation");
    }

    // ハートビート監視はワーカーと同じ apalis 用プールを読む。
    // router へ state を渡す前に取り出しておく。
    let liveness_pool = state.pg_pool.clone();

    let api = router
        .merge(Scalar::with_url("/scalar", openapi.clone()))
        .with_state(state.clone())
        .layer(cors)
        .layer(middleware::from_fn_with_state(
            state,
            handler::middlewares::csrf::csrf_origin_check,
        ))
        .layer(middleware::from_fn(logging_middleware))
        .layer(SessionLayer::new(session_store));

    let app = Router::new().merge(api);

    // ハートビート監視もワーカーと同じ扱いで見張る（監視が黙って消えないように）。
    let watched: Vec<job::liveness::WatchedWorker> =
        workers.iter().map(|(_, worker)| worker.watched()).collect();
    let mut tasks: Vec<SupervisedTask> = workers
        .into_iter()
        .map(|(label, worker)| SupervisedTask::new(label, worker.handle))
        .collect();

    if !watched.is_empty() {
        let pool = liveness_pool.clone();
        let config = job::liveness::LivenessConfig::from_env();
        let monitor_shutdown = shutdown_rx.clone();
        tasks.push(SupervisedTask::new(
            "worker heartbeat monitor",
            tokio::spawn(async move {
                job::liveness::watch_heartbeats(pool, watched, config, monitor_shutdown).await
            }),
        ));
    }

    // 監視は別タスクに置き、停止要求より先に終わったものだけを失敗として通知する。
    let (failure_tx, failure_rx) = tokio::sync::oneshot::channel::<String>();
    let (drained_tx, drained_rx) = tokio::sync::oneshot::channel::<()>();
    let watch_shutdown = shutdown_rx.clone();
    let worker_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut watcher = TaskWatcher::new(tasks);
        let failed = tokio::select! {
            reason = watcher.first_exit() => {
                // shutdown 通知と worker の正常終了は同時に ready になり得る。
                // 終了通知後のフラグ確認で、停止要求に伴う終了を failure と区別する。
                if is_unexpected_worker_exit(&worker_shutdown) {
                    let _ = failure_tx.send(reason);
                    true
                } else {
                    false
                }
            }
            _ = wait_for_shutdown(watch_shutdown) => false,
        };
        if failed {
            // 異常経路だけ期限を切る。戻ってこないジョブを待ち続けると
            // `drained_rx` が解決せず、非ゼロ終了に到達できない
            // （理由は `supervision::DRAIN_DEADLINE`）。
            watcher.drain_within(DRAIN_DEADLINE).await;
        } else {
            // 正常な停止は在庫を捌き切るまで待つ。猶予は stop_grace_period。
            watcher.drain().await;
        }
        let _ = drained_tx.send(());
    });

    let failure_reason: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    let failure_reason_after = failure_reason.clone();

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // ワーカーが先に落ちた場合も HTTP を畳む。プロセスを非ゼロ終了させて
            // restart policy に復帰させるためで、生き残っても誰もキューを消費しない。
            tokio::select! {
                () = shutdown_signal_inner() => {}
                Ok(reason) = failure_rx => {
                    warn!("{reason}; shutting down the API so the restart policy takes over");
                    *failure_reason.lock().expect("failure lock poisoned") = Some(reason);
                }
            }
            let _ = shutdown_tx.send(true);
            info!("shutting down HTTP server; Apalis workers finishing in-flight jobs");
        })
        .await?;

    // ワーカーの後始末が終わるまで待ってから終了コードを決める。
    let _ = drained_rx.await;

    if let Some(reason) = failure_reason_after
        .lock()
        .expect("failure lock poisoned")
        .take()
    {
        return Err(std::io::Error::other(reason).into());
    }

    Ok(())
}

async fn shutdown_signal_inner() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        sigterm.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => warn!("received Ctrl+C"),
        () = terminate => warn!("received SIGTERM"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// shutdown 通知を受けて正常終了した worker が、監視側の failure に化けない
    /// ことを、通知と終了が競合する形で繰り返し確認する。
    #[tokio::test]
    async fn worker_exit_after_shutdown_is_not_reported_as_failure() {
        for attempt in 0..32 {
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let mut watcher = TaskWatcher::new(vec![SupervisedTask::new(
                "compare build worker",
                tokio::spawn({
                    let mut shutdown = shutdown_rx.clone();
                    async move {
                        let _ = shutdown.changed().await;
                        Ok::<(), std::io::Error>(())
                    }
                }),
            )]);

            shutdown_tx.send(true).expect("shutdown receiver exists");
            let _reason = watcher.first_exit().await;
            assert!(
                !is_unexpected_worker_exit(&shutdown_rx),
                "graceful worker exit was reported as failure (attempt {attempt})"
            );
            watcher.drain().await;
        }
    }
}
