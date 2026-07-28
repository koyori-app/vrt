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
