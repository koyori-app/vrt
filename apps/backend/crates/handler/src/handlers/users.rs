use axum::{Json, extract::State};

use crate::AppState;
use crate::error::{AppError, ServerError};
use crate::extractors::AuthUser;
use crate::openapi::SessionAuthErrors;
use payload::users::{MeResponse, UpdateMeRequest};
use service::users as user_service;

/// ログイン中ユーザー自身のプロフィール。
///
/// セッション・PAT のどちらでも参照でき、スコープ要求は無い
/// （トークンの疎通確認に使えるようにするため）。
#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/me",
    tag = "Users",
    summary = "ログイン中ユーザーを取得",
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "ログイン中のユーザー", body = MeResponse),
        SessionAuthErrors,
    )
)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<MeResponse>, AppError> {
    let user = user_service::get_user(&state.db, auth.user_id).await?;

    Ok(Json(MeResponse::from(user)))
}

/// ログイン中ユーザー自身のプロフィールを更新する。
///
/// 表示言語は本人だけが変えられる設定なので、PAT ではなくセッションを要求する
/// （PAT は CI から使うもので、画面の言語を触る用途が無い）。
#[axum::debug_handler]
#[utoipa::path(
    patch,
    path = "/me",
    tag = "Users",
    summary = "ログイン中ユーザーを更新",
    description = "`language` は画面表示に使う言語。`null` を指定すると未設定へ戻り、\
                   画面はブラウザの言語設定に従う。対応していない言語タグはボディの\
                   deserialize で弾かれ 422。フィールドを省略すると据え置き。\
                   PAT では変更できない（403）。",
    security(("bearerAuth" = [])),
    request_body = UpdateMeRequest,
    responses(
        (status = 200, description = "更新後のユーザー", body = MeResponse),
        (status = 422, description = "対応していない言語タグです", body = ServerError),
        SessionAuthErrors,
    )
)]
pub async fn update_me(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(payload): Json<UpdateMeRequest>,
) -> Result<Json<MeResponse>, AppError> {
    auth.require_session()?;
    let user = user_service::get_user(&state.db, auth.user_id).await?;

    let Some(language) = payload.language else {
        // 変更対象が無いボディは読み取りと同じ——余計な UPDATE を打たない。
        return Ok(Json(MeResponse::from(user)));
    };

    let updated = user_service::set_language(
        &state.db,
        user,
        language.map(|language| language.as_str().to_string()),
    )
    .await?;

    Ok(Json(MeResponse::from(updated)))
}
