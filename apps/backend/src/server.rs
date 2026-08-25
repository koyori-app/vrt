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
        settings: Arc::new(state.settings.clone()),
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
) -> JoinHandle<Result<(), apalis::prelude::WorkerError>> {
    let storage = state.compare_build_storage.as_ref().clone();
    let job_state = job_state_from(state);

    // キューはストレージの設定から取る（テストはキューを差し替えるため定数を使わない）。
    let queue = storage.config().queue().to_string();
    let name = worker_name(&queue);
    info!(%queue, worker = %name, "starting compare_build worker");

    let worker = WorkerBuilder::new(name)
        .backend(storage)
        .retry(RetryPolicy::retries(compare_build::MAX_RETRIES))
        .enable_tracing()
        .concurrency(compare_build::WORKER_CONCURRENCY)
        .data(job_state)
        .build(compare_build::process);

    tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await })
}

/// `GithubStatusJob` のワーカーを spawn する（ワーカー名の一意性は [`worker_name`] 参照）。
pub fn spawn_github_status_worker(
    state: &AppState,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<Result<(), apalis::prelude::WorkerError>> {
    let storage = state.github_status_storage.as_ref().clone();
    let job_state = job_state_from(state);

    let queue = storage.config().queue().to_string();
    let name = worker_name(&queue);
    info!(%queue, worker = %name, "starting github_status worker");

    let worker = WorkerBuilder::new(name)
        .backend(storage)
        .retry(RetryPolicy::retries(github_status::MAX_RETRIES))
        .enable_tracing()
        .concurrency(github_status::WORKER_CONCURRENCY)
        .data(job_state)
        .build(github_status::process);

    tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await })
}

/// `RenderBuildJob` のワーカーを spawn する（ワーカー名の一意性は [`worker_name`] 参照）。
///
/// Chromium を起動するため 1 ジョブが重い。ジョブの同時実行数は
/// [`render_build::WORKER_CONCURRENCY`]（= 1）に絞り、ジョブ内の story だけを
/// [`render_build::STORY_RENDER_CONCURRENCY`]（= 2）で並列化する。
pub fn spawn_render_build_worker(
    state: &AppState,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<Result<(), apalis::prelude::WorkerError>> {
    let job_state = render_job_state_from(state)
        .expect("spawn_render_build_worker requires a configured CHROMIUM_PATH");
    spawn_render_build_worker_with_state(state.render_build_storage.clone(), job_state, shutdown)
}

/// HTTP API から独立した runner も利用できる Render worker の共通起動口。
pub fn spawn_render_build_worker_with_state(
    storage: Arc<RenderBuildStorage>,
    job_state: RenderJobState,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<Result<(), apalis::prelude::WorkerError>> {
    let storage = storage.as_ref().clone();

    let queue = storage.config().queue().to_string();
    let name = worker_name(&queue);
    info!(%queue, worker = %name, "starting render_build worker");

    let worker = WorkerBuilder::new(name)
        .backend(storage)
        .retry(RetryPolicy::retries(render_build::MAX_RETRIES))
        .enable_tracing()
        .concurrency(render_build::WORKER_CONCURRENCY)
        .data(job_state)
        .build(render_build::process);

    tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await })
}

/// `GithubWebhookJob` のワーカーを spawn する。
pub fn spawn_github_webhook_worker(
    state: &AppState,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<Result<(), apalis::prelude::WorkerError>> {
    let storage = state.github_webhook_storage.as_ref().clone();
    let job_state = job_state_from(state);

    let queue = storage.config().queue().to_string();
    let name = worker_name(&queue);
    info!(%queue, worker = %name, "starting github_webhook worker");

    let worker = WorkerBuilder::new(name)
        .backend(storage)
        .retry(RetryPolicy::retries(github_webhook::MAX_RETRIES))
        .enable_tracing()
        .concurrency(github_webhook::WORKER_CONCURRENCY)
        .data(job_state)
        .build(github_webhook::process);

    tokio::spawn(async move { worker.run_until(wait_for_shutdown(shutdown)).await })
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) -> Result<(), std::io::Error> {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
    Ok(())
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
    let mut worker_handles = vec![
        (
            "compare build",
            spawn_compare_build_worker(&state, shutdown_rx.clone()),
        ),
        (
            "github status",
            spawn_github_status_worker(&state, shutdown_rx.clone()),
        ),
        (
            "github webhook",
            spawn_github_webhook_worker(&state, shutdown_rx.clone()),
        ),
    ];

    if state.settings.render_worker_enabled && state.settings.storybook_render_enabled() {
        if state.settings.chromium_configured() {
            worker_handles.push((
                "render build",
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

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal_inner().await;
            let _ = shutdown_tx.send(true);
            info!("shutting down HTTP server; Apalis workers finishing in-flight jobs");
        })
        .await?;

    for (label, handle) in worker_handles {
        match handle.await {
            Ok(Ok(())) => info!("{label} worker stopped"),
            Ok(Err(e)) => warn!("{label} worker error: {e}"),
            Err(e) => warn!("{label} worker join error: {e}"),
        }
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
