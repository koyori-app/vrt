//! パーソナルアクセストークン (PAT) の CRUD。
//!
//! すべてセッション専用（PAT から PAT を発行させない）。CSRF は Origin 検査ミドルウェアが担当。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
};
use validator::Validate;

use crate::AppState;
use crate::error::AppError;
use crate::extractors::AuthUser;
use crate::openapi::{CrudErrors, SessionAuthErrors};
use entity::personal_tokens;
use entity::scopes::ScopeList;
use payload::personal_tokens::*;
use service::auth;

async fn get_owned_token(
    state: &AppState,
    token_id: Uuid,
    user_id: Uuid,
) -> Result<personal_tokens::Model, AppError> {
    personal_tokens::Entity::find_by_id(token_id)
        .filter(personal_tokens::Column::UserId.eq(user_id))
        .one(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/",
    tag = "Personal Tokens",
    summary = "自分のトークン一覧",
    responses(
        (status = 200, description = "トークン一覧", body = Vec<PersonalTokenResponse>),
        SessionAuthErrors,
    )
)]
pub async fn list_personal_tokens(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<PersonalTokenResponse>>, AppError> {
    auth.require_session()?;

    let tokens = personal_tokens::Entity::find()
        .filter(personal_tokens::Column::UserId.eq(auth.user_id))
        .order_by_desc(personal_tokens::Column::CreatedAt)
        .all(&state.db)
        .await?;

    Ok(Json(
        tokens
            .into_iter()
            .map(PersonalTokenResponse::from)
            .collect(),
    ))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/",
    tag = "Personal Tokens",
    summary = "パーソナルアクセストークンを発行",
    request_body = CreatePersonalTokenRequest,
    responses(
        (
            status = 201,
            description = "発行したトークン（平文トークンはこの応答でのみ返却）",
            body = CreatePersonalTokenResponse
        ),
        SessionAuthErrors,
    )
)]
pub async fn create_personal_token(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(payload): Json<CreatePersonalTokenRequest>,
) -> Result<(StatusCode, Json<CreatePersonalTokenResponse>), AppError> {
    auth.require_session()?;
    payload
        .validate()
        .map_err(|e| AppError::BadRequestDetail(e.to_string()))?;

    let secret = &state.settings.personal_token_secret;
    let (token_value, token_hash) =
        auth::generate_personal_token(secret).map_err(|e| AppError::Internal(e.into()))?;

    let model = personal_tokens::ActiveModel {
        id: Set(Uuid::new_v4()),
        name: Set(payload.name),
        token_last_four: Set(auth::token_last_four(&token_value)),
        token_hash: Set(token_hash),
        expires_at: Set(payload.expires_at.map(Into::into)),
        last_used_at: Set(None),
        revoked: Set(false),
        user_id: Set(auth.user_id),
        scopes: Set(ScopeList(payload.scopes)),
        created_at: Set(chrono::Utc::now().fixed_offset()),
    }
    .insert(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatePersonalTokenResponse::new(token_value, model)),
    ))
}

#[axum::debug_handler]
#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Personal Tokens",
    summary = "指定したトークンを取り消し",
    params(("id" = Uuid, Path, description = "トークンの識別子")),
    responses(
        (status = 204, description = "取り消しました"),
        CrudErrors,
    )
)]
pub async fn revoke_personal_token(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    auth.require_session()?;
    let token = get_owned_token(&state, id, auth.user_id).await?;

    if token.revoked {
        return Ok(StatusCode::NO_CONTENT);
    }

    let mut active: personal_tokens::ActiveModel = token.into();
    active.revoked = Set(true);
    active.update(&state.db).await?;

    Ok(StatusCode::NO_CONTENT)
}
