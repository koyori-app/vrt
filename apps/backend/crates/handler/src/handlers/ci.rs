//! CI クライアント向けエンドポイント。
//!
//! すべて PAT（`Authorization: Bearer`）で叩かれる想定。CSRF ミドルウェアは
//! Bearer 付きリクエストの Origin 検査をスキップするため、CI からそのまま呼べる。
//!
//! CI は UUID を知らないので、ビルド作成だけ `{tenant_slug}/{project_slug}` で指す。
//! それ以降は返ってきた `build_id` を使う。

use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::StatusCode,
};
use bytes::{Bytes, BytesMut};
use sea_orm::prelude::Uuid;
use serde::Serialize;
use utoipa::ToSchema;
use validator::Validate;

use crate::AppState;
use crate::error::{AppError, ServerError};
use crate::extractors::AuthUser;
use crate::handlers::builds::load_build_with_role;
use crate::openapi::{CrudErrors, SessionAuthErrors};
use entity::{builds::BuildStatus, scopes::Scope, tenant_members::TenantRole};
use payload::builds::*;
use service::builds as build_service;
use service::projects as project_service;
use service::screenshots::{self as screenshot_service, MAX_UPLOAD_BYTES};
use service::tenants as tenant_service;

#[derive(Serialize, ToSchema)]
pub struct CiPingResponse {
    #[schema(example = "ok")]
    pub status: String,
    #[schema(value_type = String, format = "uuid")]
    pub user_id: Uuid,
}

/// CI 用 PAT の疎通確認。`write:build` スコープを要求する。
#[axum::debug_handler(state = crate::AppState)]
#[utoipa::path(
    get,
    path = "/ping",
    tag = "CI",
    summary = "CI トークンの疎通確認",
    description = "`write:build` スコープを持つ PAT（またはセッション）でのみ 200 を返す。",
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "トークンは有効", body = CiPingResponse),
        SessionAuthErrors,
    )
)]
pub async fn ping(auth: AuthUser) -> Result<Json<CiPingResponse>, AppError> {
    auth.require_scope(Scope::WriteBuild)?;
    Ok(Json(CiPingResponse {
        status: "ok".to_string(),
        user_id: auth.user_id,
    }))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/projects/{tenant_slug}/{project_slug}/builds",
    tag = "CI",
    summary = "ビルドを作成する",
    description = "`write:build` スコープと、対象テナントのメンバーシップが必要。\
                   作成直後は `pending` で、スクリーンショットのアップロードを受け付ける。",
    params(
        ("tenant_slug" = String, Path, description = "テナント slug"),
        ("project_slug" = String, Path, description = "プロジェクト slug"),
    ),
    request_body = CreateBuildRequest,
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "作成されたビルド", body = BuildResponse),
        (status = 400, description = "リクエストが不正です", body = ServerError),
        CrudErrors,
    )
)]
pub async fn create_build(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((tenant_slug, project_slug)): Path<(String, String)>,
    Json(payload): Json<CreateBuildRequest>,
) -> Result<(StatusCode, Json<BuildResponse>), AppError> {
    auth.require_scope(Scope::WriteBuild)?;
    payload
        .validate()
        .map_err(|e| AppError::BadRequestDetail(e.to_string()))?;

    // 存在しないプロジェクトも、所属していないテナントのプロジェクトも 403 に揃える。
    let project = project_service::get_project_by_slug(&state.db, &tenant_slug, &project_slug)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    tenant_service::require_role(
        &state.db,
        project.tenant_id,
        auth.user_id,
        TenantRole::Member,
    )
    .await?;

    let build = build_service::create_build(
        &state.db,
        project.id,
        payload.branch,
        payload.commit_sha,
        payload.commit_message,
        payload.pull_request_number,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(build.into())))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/builds/{build_id}/screenshots",
    tag = "CI",
    summary = "スクリーンショットをアップロードする",
    description = "multipart/form-data。`name` フィールドを `file` より**前**に送ること。\
                   PNG のみ受け付ける（最大 25MB / 10000x10000）。\
                   `pending` 以外のビルドへのアップロードは 409、同名の重複も 409。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "保存されたスクリーンショット", body = ScreenshotResponse),
        (status = 400, description = "PNG ではない / フィールドが足りません", body = ServerError),
        (status = 409, description = "ビルドが pending ではない、または同名が既に存在します", body = ServerError),
        (status = 413, description = "ファイルが大きすぎます", body = ServerError),
        CrudErrors,
    )
)]
pub async fn upload_screenshot(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ScreenshotResponse>), AppError> {
    auth.require_scope(Scope::WriteBuild)?;
    let (build, project) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;

    // finalize 後のアップロードは受け付けない。
    if build.status != BuildStatus::Pending {
        return Err(AppError::Conflict);
    }

    let mut name: Option<String> = None;
    let mut file: Option<Bytes> = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequestDetail(format!("invalid multipart body: {e}")))?
    {
        match field.name() {
            Some("name") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequestDetail(format!("invalid name field: {e}")))?;
                name = Some(text);
            }
            Some("file") => {
                // 25MB を超えた時点で打ち切る（全部読んでから弾かない）。
                let mut buf = BytesMut::new();
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| AppError::BadRequestDetail(format!("invalid file field: {e}")))?
                {
                    if buf.len() + chunk.len() > MAX_UPLOAD_BYTES {
                        return Err(AppError::ContentTooLarge);
                    }
                    buf.extend_from_slice(&chunk);
                }
                file = Some(buf.freeze());
            }
            _ => {}
        }
    }

    let name = name.ok_or_else(|| AppError::BadRequestDetail("name field is required".into()))?;
    let file = file.ok_or_else(|| AppError::BadRequestDetail("file field is required".into()))?;

    let screenshot = screenshot_service::store_screenshot(
        &state.db,
        &state.storage,
        project.tenant_id,
        project.id,
        build.id,
        name,
        file,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(screenshot.into())))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/builds/{build_id}/finalize",
    tag = "CI",
    summary = "アップロードを締めて比較ジョブを投入する",
    description = "`pending → processing` に遷移し、`CompareBuildJob` を投入する。\
                   以降は `GET /v1/ci/builds/{build_id}` をポーリングして結果を待つ。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "processing に遷移したビルド", body = BuildResponse),
        (status = 409, description = "既に finalize 済みです", body = ServerError),
        CrudErrors,
    )
)]
pub async fn finalize_build(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_scope(Scope::WriteBuild)?;
    let (build, _) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;

    let build = build_service::finalize(&state.db, build).await?;

    job::compare_build::enqueue(
        &state.compare_build_storage,
        job::CompareBuildJob { build_id: build.id },
    )
    .await
    .map_err(AppError::Internal)?;

    // `processing` を GitHub の pending ステータスとして先に見せる。
    // 連携が無ければジョブ側が何もせず終わる。
    job::github_status::enqueue_best_effort(&state.github_status_storage, build.id).await;

    Ok(Json(build.into()))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/builds/{build_id}",
    tag = "CI",
    summary = "ビルドの状態を取得する（CI のポーリング用）",
    description = "`read:build` スコープが必要。`status` が終端になるまで CI がポーリングする。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "ビルドの状態とカウント", body = BuildResponse),
        CrudErrors,
    )
)]
pub async fn get_build_status(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let (build, _) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;
    Ok(Json(build.into()))
}
