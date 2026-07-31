//! 比較結果のレビューと差分画像配信。

use axum::{
    Json,
    extract::{Path, State},
    response::Response,
};
use sea_orm::prelude::Uuid;

use crate::AppState;
use crate::error::{AppError, ServerError};
use crate::extractors::AuthUser;
use crate::handlers::builds::load_comparison_with_role;
use crate::handlers::content::png_response;
use crate::openapi::CrudErrors;
use entity::{scopes::Scope, tenant_members::TenantRole};
use payload::comparisons::*;
use service::comparisons::{self as comparison_service, ReviewAction};

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/{comparison_id}/review",
    tag = "Comparisons",
    summary = "比較結果をレビューする",
    description = "ビルドが `changes_detected` のときだけ受け付ける。\
                   `unchanged` の比較は自動承認済みのためレビューできない（409）。",
    params(("comparison_id" = Uuid, Path, description = "比較ID")),
    request_body = ReviewComparisonRequest,
    responses(
        (status = 200, description = "レビュー後の比較結果", body = ComparisonResponse),
        (status = 409, description = "レビューできない状態です", body = ServerError),
        CrudErrors,
    )
)]
pub async fn review_comparison(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(comparison_id): Path<Uuid>,
    Json(payload): Json<ReviewComparisonRequest>,
) -> Result<Json<ComparisonResponse>, AppError> {
    auth.require_session()?;
    let (comparison, build, _) =
        load_comparison_with_role(&state, comparison_id, auth.user_id, TenantRole::Member).await?;

    let action = match payload.action {
        ReviewActionRequest::Approve => ReviewAction::Approve,
        ReviewActionRequest::Reject => ReviewAction::Reject,
    };

    let updated =
        comparison_service::review(&state.db, build.id, comparison.id, action, auth.user_id)
            .await?;
    Ok(Json(updated.into()))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{comparison_id}/diff-content",
    tag = "Comparisons",
    summary = "差分画像を取得",
    description = "差分が検出された比較にのみ差分画像がある。無い場合は 404。",
    params(("comparison_id" = Uuid, Path, description = "比較ID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "PNG 画像", content_type = "image/png"),
        CrudErrors,
    )
)]
pub async fn get_diff_content(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(comparison_id): Path<Uuid>,
) -> Result<Response, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let (comparison, _, _) =
        load_comparison_with_role(&state, comparison_id, auth.user_id, TenantRole::Member).await?;

    let key = comparison.diff_storage_key.ok_or(AppError::NotFound)?;
    png_response(&state, &key).await
}
