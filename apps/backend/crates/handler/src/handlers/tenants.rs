//! テナントとメンバー管理の HTTP ハンドラ。
//!
//! 認可の層構造:
//! 1. `AuthUser` で認証（セッション or PAT）
//! 2. 管理系はセッション専用（`require_session`）、参照系は PAT にも `read:project` で開放
//! 3. [`service::tenants::require_role`] でテナント内ロールを検査（非メンバーは一律 403）
//!
//! CSRF は Origin 検査ミドルウェアが担当する。

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
use crate::openapi::{CrudErrors, SessionAuthErrors};
use entity::{scopes::Scope, tenant_members::TenantRole};
use payload::tenants::*;
use service::tenants as tenant_service;

fn validate<T: Validate>(payload: &T) -> Result<(), AppError> {
    payload
        .validate()
        .map_err(|e| AppError::BadRequestDetail(e.to_string()))
}

// ── tenants ─────────────────────────────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/",
    tag = "Tenants",
    summary = "自分が所属するテナント一覧",
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "テナント一覧", body = Vec<TenantResponse>),
        SessionAuthErrors,
    )
)]
pub async fn list_tenants(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<TenantResponse>>, AppError> {
    auth.require_scope(Scope::ReadProject)?;
    let tenants = tenant_service::list_tenants_for_user(&state.db, auth.user_id).await?;
    Ok(Json(
        tenants
            .into_iter()
            .map(|(tenant, role)| TenantResponse::with_role(tenant, role))
            .collect(),
    ))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/",
    tag = "Tenants",
    summary = "テナントを作成",
    description = "作成者は自動的に owner になる。セッション専用（PAT からは作成できない）。",
    request_body = CreateTenantRequest,
    responses(
        (status = 201, description = "作成されたテナント", body = TenantResponse),
        (status = 400, description = "slug の書式が不正 / 予約語", body = ServerError),
        (status = 409, description = "slug が既に使われています", body = ServerError),
        SessionAuthErrors,
    )
)]
pub async fn create_tenant(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(payload): Json<CreateTenantRequest>,
) -> Result<(StatusCode, Json<TenantResponse>), AppError> {
    auth.require_session()?;
    validate(&payload)?;

    let tenant = tenant_service::create_tenant(
        &state.db,
        auth.user_id,
        payload.name,
        payload.slug,
        payload.avatar_url,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(TenantResponse::with_role(tenant, TenantRole::Owner)),
    ))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{tenant_id}",
    tag = "Tenants",
    summary = "テナントを取得",
    params(("tenant_id" = Uuid, Path, description = "テナントID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "テナント情報", body = TenantResponse),
        CrudErrors,
    )
)]
pub async fn get_tenant(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(tenant_id): Path<Uuid>,
) -> Result<Json<TenantResponse>, AppError> {
    auth.require_scope(Scope::ReadProject)?;
    let actor =
        tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Member)
            .await?;
    let tenant = tenant_service::get_tenant(&state.db, tenant_id).await?;
    Ok(Json(TenantResponse::with_role(tenant, actor.role)))
}

#[axum::debug_handler]
#[utoipa::path(
    patch,
    path = "/{tenant_id}",
    tag = "Tenants",
    summary = "テナントを更新",
    description = "admin 以上が必要。",
    params(("tenant_id" = Uuid, Path, description = "テナントID")),
    request_body = UpdateTenantRequest,
    responses(
        (status = 200, description = "更新後のテナント", body = TenantResponse),
        CrudErrors,
    )
)]
pub async fn update_tenant(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(tenant_id): Path<Uuid>,
    Json(payload): Json<UpdateTenantRequest>,
) -> Result<Json<TenantResponse>, AppError> {
    auth.require_session()?;
    validate(&payload)?;
    let actor =
        tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Admin).await?;

    let avatar_url = if payload.clear_avatar_url {
        Some(None)
    } else {
        payload.avatar_url.map(Some)
    };
    let tenant =
        tenant_service::update_tenant(&state.db, tenant_id, payload.name, avatar_url).await?;
    Ok(Json(TenantResponse::with_role(tenant, actor.role)))
}

#[axum::debug_handler]
#[utoipa::path(
    delete,
    path = "/{tenant_id}",
    tag = "Tenants",
    summary = "テナントを削除",
    description = "owner のみ。配下のメンバー・プロジェクトも削除される。",
    params(("tenant_id" = Uuid, Path, description = "テナントID")),
    responses(
        (status = 204, description = "削除しました"),
        CrudErrors,
    )
)]
pub async fn delete_tenant(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(tenant_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    auth.require_session()?;
    tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Owner).await?;
    tenant_service::delete_tenant(&state.db, tenant_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── members ─────────────────────────────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/",
    tag = "Tenant Members",
    summary = "テナントメンバー一覧",
    params(("tenant_id" = Uuid, Path, description = "テナントID")),
    responses(
        (status = 200, description = "メンバー一覧", body = Vec<TenantMemberResponse>),
        CrudErrors,
    )
)]
pub async fn list_members(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(tenant_id): Path<Uuid>,
) -> Result<Json<Vec<TenantMemberResponse>>, AppError> {
    auth.require_session()?;
    tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Member).await?;
    let members = tenant_service::list_members(&state.db, tenant_id).await?;
    Ok(Json(members.into_iter().map(Into::into).collect()))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/",
    tag = "Tenant Members",
    summary = "テナントメンバーを追加",
    description = "admin 以上が必要。owner ロールを付与できるのは owner のみ。",
    params(("tenant_id" = Uuid, Path, description = "テナントID")),
    request_body = AddMemberRequest,
    responses(
        (status = 201, description = "追加されたメンバー", body = TenantMemberResponse),
        (status = 409, description = "既にメンバーです", body = ServerError),
        CrudErrors,
    )
)]
pub async fn add_member(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(tenant_id): Path<Uuid>,
    Json(payload): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<TenantMemberResponse>), AppError> {
    auth.require_session()?;
    validate(&payload)?;
    let actor =
        tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Admin).await?;

    if payload.role == TenantRole::Owner && actor.role != TenantRole::Owner {
        return Err(AppError::Forbidden);
    }

    let member = tenant_service::add_member(
        &state.db,
        tenant_id,
        payload.user_id,
        payload.username,
        payload.role,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(member.into())))
}

#[axum::debug_handler]
#[utoipa::path(
    patch,
    path = "/{user_id}",
    tag = "Tenant Members",
    summary = "テナントメンバーのロールを変更",
    description = "admin 以上が必要。owner の付与・剥奪は owner のみ。最後の owner は降格できない。",
    params(
        ("tenant_id" = Uuid, Path, description = "テナントID"),
        ("user_id" = Uuid, Path, description = "対象ユーザーID"),
    ),
    request_body = UpdateMemberRequest,
    responses(
        (status = 200, description = "更新後のメンバー", body = TenantMemberResponse),
        (status = 409, description = "最後の owner は降格できません", body = ServerError),
        CrudErrors,
    )
)]
pub async fn update_member(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((tenant_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateMemberRequest>,
) -> Result<Json<TenantMemberResponse>, AppError> {
    auth.require_session()?;
    validate(&payload)?;
    let actor =
        tenant_service::require_role(&state.db, tenant_id, auth.user_id, TenantRole::Admin).await?;

    let target = tenant_service::find_membership(&state.db, tenant_id, target_user_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // owner ロールの付与・剥奪は owner の専権事項。
    if (payload.role == TenantRole::Owner || target.role == TenantRole::Owner)
        && actor.role != TenantRole::Owner
    {
        return Err(AppError::Forbidden);
    }

    let updated =
        tenant_service::update_member_role(&state.db, tenant_id, target_user_id, payload.role)
            .await?;
    Ok(Json(updated.into()))
}

#[axum::debug_handler]
#[utoipa::path(
    delete,
    path = "/{user_id}",
    tag = "Tenant Members",
    summary = "テナントメンバーを削除",
    description = "admin 以上、または自分自身の脱退。最後の owner は削除できない。",
    params(
        ("tenant_id" = Uuid, Path, description = "テナントID"),
        ("user_id" = Uuid, Path, description = "対象ユーザーID"),
    ),
    responses(
        (status = 204, description = "削除しました"),
        (status = 409, description = "最後の owner は削除できません", body = ServerError),
        CrudErrors,
    )
)]
pub async fn remove_member(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((tenant_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    auth.require_session()?;

    // 自分自身の脱退はメンバーなら誰でもできる。他人を外すのは admin 以上。
    let min_role = if target_user_id == auth.user_id {
        TenantRole::Member
    } else {
        TenantRole::Admin
    };
    let actor = tenant_service::require_role(&state.db, tenant_id, auth.user_id, min_role).await?;

    let target = tenant_service::find_membership(&state.db, tenant_id, target_user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if target.role == TenantRole::Owner && actor.role != TenantRole::Owner {
        return Err(AppError::Forbidden);
    }

    tenant_service::remove_member(&state.db, tenant_id, target_user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
