use axum::{Json, extract::State};
use sea_orm::EntityTrait;

use crate::AppState;
use crate::error::AppError;
use crate::extractors::AuthUser;
use crate::openapi::SessionAuthErrors;
use payload::users::MeResponse;

/// ログイン中ユーザー自身のプロフィール。
///
/// セッション・PAT のどちらでも参照でき、スコープ要求は無い
/// （トークンの疎通確認に使えるようにするため）。
#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/me",
    tag = "Users",
    summary = "ログイン中ユーザーを取得",
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "ログイン中のユーザー", body = MeResponse),
        SessionAuthErrors,
    )
)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<MeResponse>, AppError> {
    let user = entity::users::Entity::find_by_id(auth.user_id)
        .one(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(MeResponse::from(user)))
}
