//! 認証エクストラクタ（task から移植し VRT のスコープに合わせて調整）。
//!
//! - [`AuthUser`]: セッション Cookie または PAT（`Authorization: Bearer`）
//! - [`CurrentUser`]: セッション専用（PAT では通さない）

use std::ops::Deref;

use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use axum_session_redispool::SessionRedisPool;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, prelude::Uuid};

use entity::{scopes::Scope, scopes::ScopeList, users};

use crate::{AppState, error::AppError};
use service::auth::{AuthError, authenticate_personal_token, touch_personal_token_last_used};

pub type Session = axum_session::Session<SessionRedisPool>;

/// セッションに保存するユーザー ID のキー。
pub const SESSION_USER_ID_KEY: &str = "user_id";
/// セッション発行時刻（ミリ秒）。`users.sessions_revoked_at` との比較に使う。
pub const SESSION_ISSUED_AT_KEY: &str = "issued_at_ms";

pub async fn session_from_parts(parts: &mut Parts, state: &AppState) -> Result<Session, AuthError> {
    Session::from_request_parts(parts, state)
        .await
        .map_err(|_| AuthError::Internal(anyhow::anyhow!("session layer missing")))
}

/// セッションからユーザーを解決する。グローバルログアウト後の古いセッションは弾く。
pub async fn user_from_session(
    session: &Session,
    state: &AppState,
) -> Result<users::Model, AuthError> {
    let user_id = session
        .get::<Uuid>(SESSION_USER_ID_KEY)
        .ok_or(AuthError::Unauthorized)?;
    let issued_at_ms = session.get::<i64>(SESSION_ISSUED_AT_KEY).unwrap_or(0);

    let user = users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await?
        .ok_or(AuthError::Unauthorized)?;

    if let Some(revoked_at) = user.sessions_revoked_at
        && issued_at_ms < revoked_at.timestamp_millis()
    {
        return Err(AuthError::Unauthorized);
    }

    Ok(user)
}

fn bearer_token_from_parts(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Debug, Clone)]
pub enum AuthMethod {
    Session,
    PersonalToken { token_id: Uuid, scopes: ScopeList },
}

/// 認証済みユーザー（セッションまたは PAT）
pub struct AuthUser {
    pub user_id: Uuid,
    pub method: AuthMethod,
}

impl AuthUser {
    /// PAT 管理 API などセッション専用エンドポイント向け。
    pub fn require_session(&self) -> Result<(), AppError> {
        match self.method {
            AuthMethod::Session => Ok(()),
            AuthMethod::PersonalToken { .. } => Err(AppError::Forbidden),
        }
    }

    /// 操作スコープチェック。セッション（ブラウザの本人操作）は常に通過する。
    pub fn require_scope(&self, scope: Scope) -> Result<(), AppError> {
        match &self.method {
            AuthMethod::Session => Ok(()),
            AuthMethod::PersonalToken { scopes, .. } => {
                if scopes.has_scope(scope) {
                    Ok(())
                } else {
                    Err(AppError::Forbidden)
                }
            }
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(token) = bearer_token_from_parts(parts) {
            let record = authenticate_personal_token(
                &state.db,
                &state.settings.personal_token_secret,
                &token,
            )
            .await?;

            // ユーザーが削除済みなら PAT も無効。
            let user_exists = users::Entity::find()
                .filter(users::Column::Id.eq(record.user_id))
                .one(&state.db)
                .await?
                .is_some();
            if !user_exists {
                return Err(AuthError::Unauthorized);
            }

            touch_personal_token_last_used(&state.db, record.id).await?;

            Ok(AuthUser {
                user_id: record.user_id,
                method: AuthMethod::PersonalToken {
                    token_id: record.id,
                    scopes: record.scopes.clone(),
                },
            })
        } else {
            let session = session_from_parts(parts, state).await?;
            let user = user_from_session(&session, state).await?;
            Ok(AuthUser {
                user_id: user.id,
                method: AuthMethod::Session,
            })
        }
    }
}

/// 認証任意（未認証は `None`）。画像配信など公開/非公開が混ざる経路で使う。
pub struct OptionalAuthUser(pub Option<AuthUser>);

impl FromRequestParts<AppState> for OptionalAuthUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match AuthUser::from_request_parts(parts, state).await {
            Ok(auth) => Ok(OptionalAuthUser(Some(auth))),
            Err(AuthError::Unauthorized) | Err(AuthError::Forbidden) => Ok(OptionalAuthUser(None)),
            Err(e) => Err(e),
        }
    }
}

/// セッション専用の認証済みユーザー（DB レコード付き）。PAT では通らない。
pub struct CurrentUser(pub users::Model);

impl Deref for CurrentUser {
    type Target = users::Model;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let session = session_from_parts(parts, state).await?;
        let user = user_from_session(&session, state).await?;
        Ok(CurrentUser(user))
    }
}
