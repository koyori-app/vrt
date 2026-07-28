//! OpenAPI 用の共通レスポンス型（ランタイムの `IntoResponse` とは別定義）。

#![allow(dead_code)]

use utoipa::IntoResponses;

use crate::error::ServerError;

#[derive(IntoResponses)]
pub enum SessionAuthErrors {
    #[response(status = 401, description = "ログインまたはセッションが必要です")]
    Unauthorized(#[to_schema] ServerError),
    #[response(status = 403, description = "この操作は許可されていません")]
    Forbidden(#[to_schema] ServerError),
    #[response(
        status = 500,
        description = "サーバー側で問題が発生しました。時間をおいて再度お試しください"
    )]
    Internal(#[to_schema] ServerError),
}

#[derive(IntoResponses)]
pub enum UnauthorizedErrors {
    #[response(status = 401, description = "ログインまたはセッションが必要です")]
    Unauthorized(#[to_schema] ServerError),
    #[response(
        status = 500,
        description = "サーバー側で問題が発生しました。時間をおいて再度お試しください"
    )]
    Internal(#[to_schema] ServerError),
}

#[derive(IntoResponses)]
#[response(
    status = 500,
    description = "サーバー側で問題が発生しました。時間をおいて再度お試しください"
)]
pub struct InternalOnlyError(#[to_schema] ServerError);

#[derive(IntoResponses)]
pub enum CrudErrors {
    #[response(status = 401, description = "ログインまたはセッションが必要です")]
    Unauthorized(#[to_schema] ServerError),
    #[response(status = 403, description = "この操作は許可されていません")]
    Forbidden(#[to_schema] ServerError),
    #[response(status = 404, description = "リソースが見つかりません")]
    NotFound(#[to_schema] ServerError),
    #[response(
        status = 500,
        description = "サーバー側で問題が発生しました。時間をおいて再度お試しください"
    )]
    Internal(#[to_schema] ServerError),
}

#[derive(IntoResponses)]
pub enum OAuthErrors {
    #[response(status = 302, description = "リダイレクト")]
    Redirect,
    #[response(status = 400, description = "リクエストが不正です")]
    BadRequest(#[to_schema] ServerError),
    #[response(status = 401, description = "ログインまたはセッションが必要です")]
    Unauthorized(#[to_schema] ServerError),
    #[response(status = 403, description = "この操作は許可されていません")]
    Forbidden(#[to_schema] ServerError),
    #[response(status = 404, description = "リソースが見つかりません")]
    NotFound(#[to_schema] ServerError),
    #[response(status = 409, description = "競合（連携済み等）")]
    Conflict(#[to_schema] ServerError),
    #[response(
        status = 500,
        description = "サーバー側で問題が発生しました。時間をおいて再度お試しください"
    )]
    Internal(#[to_schema] ServerError),
}
