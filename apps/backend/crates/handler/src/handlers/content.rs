//! 画像実体の配信。ストレージからのストリーミングプロキシ。
//!
//! 画像は内容が不変（ストレージキーが UUID 由来で使い回されない）なので、
//! `Cache-Control: private, max-age=..., immutable` を付けてブラウザにキャッシュさせる。
//! `private` はセッション Cookie 越しの配信であるため（共有キャッシュに載せない）。

use axum::{
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::Response,
};
use sea_orm::prelude::Uuid;

use crate::AppState;
use crate::error::AppError;
use crate::extractors::AuthUser;
use crate::handlers::builds::{load_baseline_entry_with_role, load_screenshot_with_role};
use crate::openapi::CrudErrors;
use entity::{scopes::Scope, tenant_members::TenantRole};
use service::screenshots::PNG_MIME;

/// 画像のキャッシュ有効期間（1 年）。
const CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// ストレージのオブジェクトを PNG としてストリーム配信する。
pub(crate) async fn png_response(state: &AppState, key: &str) -> Result<Response, AppError> {
    let stream = service::screenshots::open_stream(&state.storage, key).await?;
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, PNG_MIME)
        .header(header::CACHE_CONTROL, CACHE_CONTROL)
        .body(body)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build image response: {e}")))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{screenshot_id}/content",
    tag = "Screenshots",
    summary = "スクリーンショット画像を取得",
    params(("screenshot_id" = Uuid, Path, description = "スクリーンショットID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "PNG 画像", content_type = "image/png"),
        CrudErrors,
    )
)]
pub async fn get_screenshot_content(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(screenshot_id): Path<Uuid>,
) -> Result<Response, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let (shot, _) =
        load_screenshot_with_role(&state, screenshot_id, auth.user_id, TenantRole::Member).await?;
    png_response(&state, &shot.storage_key).await
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{baseline_entry_id}/content",
    tag = "Baselines",
    summary = "baseline 画像を取得",
    params(("baseline_entry_id" = Uuid, Path, description = "baseline エントリID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "PNG 画像", content_type = "image/png"),
        CrudErrors,
    )
)]
pub async fn get_baseline_entry_content(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(baseline_entry_id): Path<Uuid>,
) -> Result<Response, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let (entry, _) =
        load_baseline_entry_with_role(&state, baseline_entry_id, auth.user_id, TenantRole::Member)
            .await?;
    png_response(&state, &entry.storage_key).await
}
