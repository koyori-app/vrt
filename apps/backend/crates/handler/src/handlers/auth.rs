//! OAuth ログイン（GitHub / GitLab）とログアウト。
//!
//! auth-core は PKCE・state・トークン交換・ユーザー情報取得までを担い、
//! HTTP ハンドラとセッション発行はここで組み立てる。

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;
use thiserror::Error;
use tracing::{error, warn};
use utoipa::IntoParams;

use auth_core::client::exchange_code;
use auth_core::pkce::{generate_pkce_pair, generate_state};
use auth_core::provider::build_authorize_url;
use auth_core::state::{
    build_frontend_oauth_error_redirect, build_frontend_redirect, consume_state,
    sanitize_redirect_path, store_state,
};

use crate::AppState;
use crate::error::{ServerError, internal_server_error};
use crate::extractors::{SESSION_ISSUED_AT_KEY, SESSION_USER_ID_KEY, Session};
use crate::openapi::OAuthErrors;
use service::auth::{encrypt_oauth_token, upsert_oauth_user};
use service::oauth::{DEFAULT_REDIRECT_PATH, OAuthConfigError, OAuthStatePayload};

/// セッションに保持する発行済み state（セッション固定 / state 差し替え対策）。
const OAUTH_PENDING_STATE_KEY: &str = "oauth_pending_state";
const OAUTH_PENDING_PROVIDER_KEY: &str = "oauth_pending_provider";

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
    /// 未知のプロバイダー（ルート引数が github / gitlab 等でない）。
    #[error("unknown oauth provider")]
    UnknownProvider,
    /// 既知だがクライアント ID 未設定。
    #[error("oauth provider is not configured")]
    NotConfigured,
    #[error("invalid oauth state")]
    InvalidState,
    #[error("bad request")]
    BadRequest,
    /// 無効化されている口（テスト専用ログインなど）。存在自体を隠すため 404。
    #[error("not found")]
    NotFound,
}

impl From<sea_orm::DbErr> for OAuthError {
    fn from(err: sea_orm::DbErr) -> Self {
        OAuthError::Internal(err.into())
    }
}

impl From<service::auth::AuthError> for OAuthError {
    fn from(err: service::auth::AuthError) -> Self {
        match err {
            service::auth::AuthError::Internal(e) => OAuthError::Internal(e),
            other => OAuthError::Internal(anyhow::anyhow!("{other}")),
        }
    }
}

impl From<OAuthConfigError> for OAuthError {
    fn from(err: OAuthConfigError) -> Self {
        match err {
            OAuthConfigError::UnknownProvider => OAuthError::UnknownProvider,
            OAuthConfigError::NotConfigured => OAuthError::NotConfigured,
        }
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        match self {
            OAuthError::Internal(e) => {
                error!("oauth error: {:#?}", e);
                internal_server_error().into_response()
            }
            OAuthError::UnknownProvider => (
                StatusCode::NOT_FOUND,
                Json(ServerError {
                    message: "oauth-provider-unknown".into(),
                }),
            )
                .into_response(),
            OAuthError::NotConfigured => (
                StatusCode::BAD_REQUEST,
                Json(ServerError {
                    message: "oauth-provider-not-configured".into(),
                }),
            )
                .into_response(),
            OAuthError::InvalidState => (
                StatusCode::BAD_REQUEST,
                Json(ServerError {
                    message: "invalid-oauth-state".into(),
                }),
            )
                .into_response(),
            OAuthError::BadRequest => (
                StatusCode::BAD_REQUEST,
                Json(ServerError {
                    message: "bad-request".into(),
                }),
            )
                .into_response(),
            OAuthError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(ServerError {
                    message: "not-found".into(),
                }),
            )
                .into_response(),
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OAuthLoginQuery {
    /// ログイン後に戻るフロントの相対パス（同一 origin のみ許可）。
    pub redirect_to: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{provider}/login",
    tag = "Auth",
    summary = "OAuth 認可 URL へリダイレクト",
    params(
        ("provider" = String, Path, description = "github | gitlab | gitlab_selfhosted"),
        OAuthLoginQuery,
    ),
    responses(
        (status = 307, description = "プロバイダーの認可 URL へリダイレクト"),
        OAuthErrors,
    )
)]
pub async fn oauth_login(
    Path(provider): Path<String>,
    Query(query): Query<OAuthLoginQuery>,
    session: Session,
    State(state): State<AppState>,
) -> Result<Redirect, OAuthError> {
    let registry = &state.oauth;
    let (oauth_provider, credentials) = registry.resolve(&provider)?;

    let endpoints = oauth_provider
        .endpoints(&registry.http)
        .await
        .map_err(OAuthError::Internal)?;

    let pkce = generate_pkce_pair();
    let oauth_state = generate_state();

    let raw_redirect = query
        .redirect_to
        .as_deref()
        .unwrap_or(DEFAULT_REDIRECT_PATH);
    // オープンリダイレクト対策。相対パス以外は受け付けない。
    let redirect_to = sanitize_redirect_path(raw_redirect).map_err(|e| {
        warn!("oauth redirect_to rejected: {e}");
        OAuthError::BadRequest
    })?;

    store_state(
        &registry.state_store,
        &oauth_state,
        &OAuthStatePayload {
            provider: provider.clone(),
            code_verifier: pkce.code_verifier,
            redirect_to,
        },
    )
    .await
    .map_err(OAuthError::Internal)?;

    // state をセッションにも控え、コールバックで突き合わせる。
    session.set(OAUTH_PENDING_STATE_KEY, oauth_state.clone());
    session.set(OAUTH_PENDING_PROVIDER_KEY, provider.clone());

    let authorize_url = build_authorize_url(
        &endpoints,
        &credentials.client_id,
        &registry.callback_url(&provider),
        &oauth_state,
        &pkce.code_challenge,
    );

    Ok(Redirect::temporary(&authorize_url))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{provider}/callback",
    tag = "Auth",
    summary = "OAuth コールバック",
    params(
        ("provider" = String, Path, description = "github | gitlab | gitlab_selfhosted"),
        OAuthCallbackQuery,
    ),
    responses(
        (status = 307, description = "フロントエンドへリダイレクト"),
        OAuthErrors,
    )
)]
pub async fn oauth_callback(
    Path(provider): Path<String>,
    Query(query): Query<OAuthCallbackQuery>,
    session: Session,
    State(state): State<AppState>,
) -> Result<Redirect, OAuthError> {
    let registry = &state.oauth;
    let (oauth_provider, credentials) = registry.resolve(&provider)?;

    // プロバイダー側が error を返した場合はフロントへエラー付きで戻す。
    if let Some(error) = &query.error {
        warn!(
            oauth_error = %error,
            error_description = query.error_description.as_deref().unwrap_or(""),
            provider = %provider,
            "oauth provider returned authorization error"
        );
        clear_pending_state(&session);
        return oauth_error_redirect(&registry.app_url, DEFAULT_REDIRECT_PATH);
    }

    let code = query.code.ok_or(OAuthError::BadRequest)?;
    let state_param = query.state.ok_or(OAuthError::BadRequest)?;

    // state は GETDEL で使い捨て。消費済み・期限切れならフロントへエラー付きで戻す。
    let Some(payload): Option<OAuthStatePayload> =
        consume_state(&registry.state_store, &state_param)
            .await
            .map_err(OAuthError::Internal)?
    else {
        warn!(provider = %provider, "oauth state missing or already consumed");
        clear_pending_state(&session);
        return oauth_error_redirect(&registry.app_url, DEFAULT_REDIRECT_PATH);
    };

    if payload.provider != provider {
        return Err(OAuthError::InvalidState);
    }

    // セッション固定対策: 認可を開始したブラウザと同一セッションであることを確認する。
    let pending_state: Option<String> = session.get(OAUTH_PENDING_STATE_KEY);
    let pending_provider: Option<String> = session.get(OAUTH_PENDING_PROVIDER_KEY);
    if pending_state.as_deref() != Some(state_param.as_str())
        || pending_provider.as_deref() != Some(provider.as_str())
    {
        return Err(OAuthError::InvalidState);
    }
    clear_pending_state(&session);

    let endpoints = oauth_provider
        .endpoints(&registry.http)
        .await
        .map_err(OAuthError::Internal)?;

    let token = exchange_code(
        &registry.http,
        &endpoints,
        credentials,
        &code,
        &registry.callback_url(&provider),
        &payload.code_verifier,
    )
    .await
    .map_err(OAuthError::Internal)?;

    let info = oauth_provider
        .fetch_user_info(&registry.http, &endpoints, &token.access_token)
        .await
        .map_err(OAuthError::Internal)?;

    let access_token_enc = Some(encrypt_oauth_token(
        &state.settings.oauth_token_encryption_key,
        &token.access_token,
    )?);

    let user = upsert_oauth_user(&state.db, oauth_provider.slug(), &info, access_token_enc).await?;

    // セッション固定攻撃対策: ログイン成立時に必ずセッション ID を作り直す。
    session.renew();
    session.set(SESSION_ISSUED_AT_KEY, Utc::now().timestamp_millis());
    session.set(SESSION_USER_ID_KEY, user.id);

    let redirect =
        build_frontend_redirect(&registry.app_url, &payload.redirect_to).map_err(|e| {
            warn!("oauth frontend redirect build failed: {e}");
            OAuthError::BadRequest
        })?;

    Ok(Redirect::temporary(&redirect))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/logout",
    tag = "Auth",
    summary = "ログアウト（セッション破棄）",
    responses(
        (status = 204, description = "ログアウトしました"),
    )
)]
pub async fn logout(session: Session) -> StatusCode {
    session.destroy();
    StatusCode::NO_CONTENT
}

/// テスト専用ログインのリクエスト。
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct TestLoginRequest {
    /// ログインするユーザー名。存在しなければ作成する。
    pub username: String,
}

/// **テスト専用のログイン口。本番では絶対に有効にしないこと。**
///
/// `TEST_LOGIN_ENABLED=true` のときだけ有効になり、それ以外では常に 404 を返す。
/// このリポジトリの唯一のログイン手段は OAuth であり、e2e からは OAuth プロバイダーを
/// 踏めないため、セッションを直接発行するための穴として用意している。
///
/// 二重の歯止め:
/// 1. `Settings::test_login_enabled`（既定 false）が立っていないと 404
/// 2. release ビルドではそのフラグを立てた時点で起動が失敗する
///    （`common::settings::load_settings`）
#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/test-login",
    tag = "Auth",
    summary = "テスト専用ログイン（本番では無効）",
    description = "TEST_LOGIN_ENABLED=true のときのみ有効。それ以外は 404。",
    request_body = TestLoginRequest,
    responses(
        (status = 204, description = "セッションを発行しました"),
        (status = 404, description = "無効化されています", body = ServerError),
    )
)]
pub async fn test_login(
    State(state): State<AppState>,
    session: Session,
    Json(payload): Json<TestLoginRequest>,
) -> Result<StatusCode, OAuthError> {
    if !state.settings.test_login_enabled {
        return Err(OAuthError::NotFound);
    }
    if payload.username.trim().is_empty() {
        return Err(OAuthError::BadRequest);
    }

    let user = service::auth::upsert_test_user(&state.db, &payload.username).await?;

    session.renew();
    session.set(SESSION_ISSUED_AT_KEY, Utc::now().timestamp_millis());
    session.set(SESSION_USER_ID_KEY, user.id);

    Ok(StatusCode::NO_CONTENT)
}

fn clear_pending_state(session: &Session) {
    session.remove(OAUTH_PENDING_STATE_KEY);
    session.remove(OAUTH_PENDING_PROVIDER_KEY);
}

fn oauth_error_redirect(app_url: &str, redirect_to: &str) -> Result<Redirect, OAuthError> {
    let url = build_frontend_oauth_error_redirect(app_url, redirect_to).map_err(|e| {
        warn!("oauth error redirect build failed: {e}");
        OAuthError::BadRequest
    })?;
    Ok(Redirect::temporary(&url))
}
