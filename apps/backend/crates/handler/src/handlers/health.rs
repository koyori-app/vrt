use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

/// ヘルスチェック応答。
#[derive(Serialize, ToSchema)]
pub struct HealthResponse {
    #[schema(example = "ok")]
    pub status: String,
}

/// 死活監視用エンドポイント。認証不要。
#[utoipa::path(
    get,
    path = "/health",
    tag = "Health",
    responses(
        (status = 200, description = "サーバーは正常に稼働しています", body = HealthResponse),
    )
)]
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

/// キュー 1 本の状態（`/v1/health/queues`）。
#[derive(Serialize, ToSchema)]
pub struct QueueHealthResponse {
    /// キュー名（= `apalis.workers.worker_type`）。
    pub queue: String,
    /// 直近にハートビートを打ったワーカー数。0 なら誰も消費していない。
    pub live_workers: i64,
    /// 最新ハートビートの古さ（秒）。ワーカーの登録が無ければ null。
    #[schema(nullable)]
    pub newest_heartbeat_age_seconds: Option<u64>,
    /// 一度も取得されていない待ちジョブ数。
    pub waiting_jobs: i64,
    /// その最古の待ち時間（秒）。待ちが無ければ null。
    #[schema(nullable)]
    pub oldest_wait_seconds: Option<u64>,
}

/// `/v1/health/queues` の応答。
#[derive(Serialize, ToSchema)]
pub struct QueuesHealthResponse {
    pub queues: Vec<QueueHealthResponse>,
}

/// ジョブキューの状態を返す。認証不要。
///
/// 外形監視はセッションを持てないので認証を要求しない。その代わり、
/// ワーカー ID や接続情報は出さず集計値だけを返す。
///
/// この値でプロセスを落とすことはしない。滞留は「ワーカーが詰まっている」
/// 場合と「単に混んでいる」場合を区別できず、混雑を障害と誤認して再起動すると
/// 孤児の再投入で `attempts` を消費して事態を悪化させる。判断は人に残す。
#[utoipa::path(
    get,
    path = "/health/queues",
    tag = "Health",
    responses(
        (status = 200, description = "キューごとのワーカーと待ちジョブの状態", body = QueuesHealthResponse),
        (status = 500, description = "キューの状態を読めませんでした", body = crate::error::ServerError),
    )
)]
pub async fn queues_health(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Result<Json<QueuesHealthResponse>, crate::error::AppError> {
    let queues = job::liveness::queue_health(
        &state.pg_pool,
        job::liveness::LivenessConfig::from_env().stale_after,
    )
    .await
    .map_err(|e| crate::error::AppError::Internal(anyhow::anyhow!("read queue health: {e}")))?;

    Ok(Json(QueuesHealthResponse {
        queues: queues
            .into_iter()
            .map(|q| QueueHealthResponse {
                queue: q.queue,
                live_workers: q.live_workers,
                newest_heartbeat_age_seconds: q.newest_heartbeat_age.map(|d| d.as_secs()),
                waiting_jobs: q.waiting_jobs,
                oldest_wait_seconds: q.oldest_wait.map(|d| d.as_secs()),
            })
            .collect(),
    }))
}
