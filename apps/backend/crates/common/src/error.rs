//! ハンドラ共通の API エラー型。

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;
use tracing::error;
use utoipa::ToSchema;

/// API 共通のエラー応答ボディ。
#[derive(Serialize, ToSchema)]
pub struct ServerError {
    #[schema(example = "internal-error")]
    pub message: String,
}

/// 認証・認可以外の一般ハンドラ向けエラー。
#[derive(Debug, Error)]
pub enum AppError {
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
    #[error("not found")]
    NotFound,
    /// 資格情報が無い / 一致しない（GitHub webhook の署名検証など）。
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("conflict")]
    Conflict,
    #[error("bad request")]
    BadRequest,
    #[error("bad request: {0}")]
    BadRequestDetail(String),
    #[error("gone")]
    Gone,
    #[error("unprocessable entity")]
    UnprocessableEntity,
    #[error("content too large")]
    ContentTooLarge,
}

impl From<sea_orm::DbErr> for AppError {
    fn from(err: sea_orm::DbErr) -> Self {
        let msg = err.to_string();
        if msg.contains("duplicate key") || msg.contains("UNIQUE constraint failed") {
            return AppError::Conflict;
        }
        AppError::Internal(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Internal(e) => {
                error!("app error: {:#?}", e);
                internal_server_error().into_response()
            }
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(ServerError {
                    message: "not-found".into(),
                }),
            )
                .into_response(),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(ServerError {
                    message: "unauthorized".into(),
                }),
            )
                .into_response(),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(ServerError {
                    message: "forbidden".into(),
                }),
            )
                .into_response(),
            AppError::Conflict => (
                StatusCode::CONFLICT,
                Json(ServerError {
                    message: "conflict".into(),
                }),
            )
                .into_response(),
            AppError::BadRequest => (
                StatusCode::BAD_REQUEST,
                Json(ServerError {
                    message: "bad-request".into(),
                }),
            )
                .into_response(),
            AppError::BadRequestDetail(msg) => {
                (StatusCode::BAD_REQUEST, Json(ServerError { message: msg })).into_response()
            }
            AppError::Gone => (
                StatusCode::GONE,
                Json(ServerError {
                    message: "gone".into(),
                }),
            )
                .into_response(),
            AppError::UnprocessableEntity => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ServerError {
                    message: "unprocessable-entity".into(),
                }),
            )
                .into_response(),
            AppError::ContentTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(ServerError {
                    message: "content-too-large".into(),
                }),
            )
                .into_response(),
        }
    }
}

/// 500 + `internal-error`（`AuthError` / `AppError` で共用）。
pub fn internal_server_error() -> (StatusCode, Json<ServerError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ServerError {
            message: "internal-error".into(),
        }),
    )
}
