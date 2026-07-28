//! プロジェクトの HTTP ハンドラ。
//!
//! テナント配下 (`/v1/tenants/{tenant_id}/projects`) と、
//! プロジェクト ID 直参照 (`/v1/projects/{project_id}`) の 2 系統がある。
//! 後者は [`load_project_with_role`] でプロジェクトを引いてから、
//! その所有テナントに対するロールを検査する（非メンバーは 403）。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use sea_orm::prelude::Uuid;
use validator::Validate;

use crate::AppState;
use crate::error::{AppError, ServerError};
use crate::extractors::AuthUser;
use crate::openapi::CrudErrors;
use entity::{projects, scopes::Scope, tenant_members, tenant_members::TenantRole};
use payload::projects::*;
use service::projects as project_service;
use service::tenants as tenant_service;

fn validate<T: Validate>(payload: &T) -> Result<(), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequestDetail(e.to_string()))
}

/// プロジェクトを読み込み、所有テナントに対する `min_role` を要求する。
///
/// 存在しないプロジェクトも、所属していないテナントのプロジェクトも 403 に揃えて、
/// 他テナントのプロジェクト ID の存在有無を漏らさない。
async fn load_project_with_role(
    state: &AppState,
    project_id: Uuid,
    user_id: Uuid,
    min_role: TenantRole,
) -> Result<(projects::Model, tenant_members::Model), AppError> {
    let project = project_service::get_project(&state.db, project_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    let member =
        tenant_service::require_role(&state.db, project.tenant_id, user_id, min_role).await?;
    Ok((project, member))
}

// ── /v1/tenants/{tenant_id}/projects ────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/",
    tag = "Projects",
    summary = "テナント内のプロジェクト一覧",
    description = "テナントのメンバーであること。PAT は `read:project` スコープで参照できる。",
    params(("tenant_id" = Uuid, Path, description = "テナントID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "プロジェクト一覧", body = Vec<ProjectResponse>),
        CrudErrors,
    )
)]
pub async fn list_projects(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(tenant_id): Path<Uuid>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    auth.require_scope(Scope::ReadProject)?;
    tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Member).await?;
    let list = project_service::list_projects(&state.db, tenant_id).await?;
    Ok(Json(list.into_iter().map(Into::into).collect()))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/",
    tag = "Projects",
    summary = "プロジェクトを作成",
    description = "admin 以上が必要。slug はテナント内で一意。",
    params(("tenant_id" = Uuid, Path, description = "テナントID")),
    request_body = CreateProjectRequest,
    responses(
        (status = 201, description = "作成されたプロジェクト", body = ProjectResponse),
        (status = 400, description = "slug の書式が不正 / 予約語", body = ServerError),
        (status = 409, description = "slug がテナント内で重複しています", body = ServerError),
        CrudErrors,
    )
)]
pub async fn create_project(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(tenant_id): Path<Uuid>,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<ProjectResponse>), AppError> {
    auth.require_session()?;
    validate(&payload)?;
    tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Admin).await?;

    let project = project_service::create_project(
        &state.db,
        tenant_id,
        payload.name,
        payload.slug,
        payload.default_branch,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(project.into())))
}

// ── /v1/projects/{project_id} ───────────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{project_id}",
    tag = "Projects",
    summary = "プロジェクトを取得",
    description = "所有テナントのメンバーであること。PAT は `read:project` スコープで参照できる。",
    params(("project_id" = Uuid, Path, description = "プロジェクトID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "プロジェクト情報", body = ProjectResponse),
        CrudErrors,
    )
)]
pub async fn get_project(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<ProjectResponse>, AppError> {
    auth.require_scope(Scope::ReadProject)?;
    let (project, _) =
        load_project_with_role(&state, project_id, auth.user_id, TenantRole::Member).await?;
    Ok(Json(project.into()))
}

#[axum::debug_handler]
#[utoipa::path(
    patch,
    path = "/{project_id}",
    tag = "Projects",
    summary = "プロジェクト設定を更新",
    description = "admin 以上が必要。`diff_threshold` / `diff_ratio_fail` は 0.0〜1.0。",
    params(("project_id" = Uuid, Path, description = "プロジェクトID")),
    request_body = UpdateProjectRequest,
    responses(
        (status = 200, description = "更新後のプロジェクト", body = ProjectResponse),
        (status = 400, description = "設定値が範囲外です", body = ServerError),
        CrudErrors,
    )
)]
pub async fn update_project(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<UpdateProjectRequest>,
) -> Result<Json<ProjectResponse>, AppError> {
    auth.require_session()?;
    validate(&payload)?;
    let (project, _) =
        load_project_with_role(&state, project_id, auth.user_id, TenantRole::Admin).await?;

    let updated = project_service::update_project(
        &state.db,
        project,
        project_service::ProjectSettings {
            name: payload.name,
            default_branch: payload.default_branch,
            diff_threshold: payload.diff_threshold,
            diff_ratio_fail: payload.diff_ratio_fail,
        },
    )
    .await?;
    Ok(Json(updated.into()))
}

#[axum::debug_handler]
#[utoipa::path(
    delete,
    path = "/{project_id}",
    tag = "Projects",
    summary = "プロジェクトを削除",
    description = "owner のみ（task のプロジェクト削除がテナントオーナー専用なのに合わせる）。",
    params(("project_id" = Uuid, Path, description = "プロジェクトID")),
    responses(
        (status = 204, description = "削除しました"),
        CrudErrors,
    )
)]
pub async fn delete_project(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    auth.require_session()?;
    let (project, _) =
        load_project_with_role(&state, project_id, auth.user_id, TenantRole::Owner).await?;
    project_service::delete_project(&state.db, project.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
