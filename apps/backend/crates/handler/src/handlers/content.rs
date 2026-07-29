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
use crate::handlers::builds::{
    load_baseline_entry_with_role, load_build_with_role, load_screenshot_with_role,
};
use crate::openapi::CrudErrors;
use entity::{builds::BuildMode, scopes::Scope, tenant_members::TenantRole};
use service::render::StorybookServeError;
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

// ── Storybook バンドルの対話配信（Open Storybook）────────────────────────
//
// `mode = storybook` のビルドにアップロードされた Storybook そのものを、静的サイトとして
// そのまま配信する（Chromatic の View Storybook 相当）。認可はビルド詳細と同じ
// （セッションのテナントメンバー、または `read:build` を持つ PAT）。フロントの `/api`
// プロキシがセッション Cookie を透過するので、`<iframe>` / `<img>` 経由でも Cookie 認証が効く。
//
// バンドルは 1 ビルドにつき不変なので immutable キャッシュを付ける。

/// ストレージのバンドルから 1 ファイルを配信する共通処理。
async fn serve_storybook(
    state: &AppState,
    auth: &AuthUser,
    build_id: Uuid,
    rel_path: &str,
) -> Result<Response, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let (build, _) =
        load_build_with_role(state, build_id, auth.user_id, TenantRole::Member).await?;

    // storybook モードでない、またはまだバンドルが上がっていないビルドは「無い」扱い。
    if build.mode != BuildMode::Storybook {
        return Err(AppError::NotFound);
    }
    let Some(key) = build.storybook_key.as_deref() else {
        return Err(AppError::NotFound);
    };

    let cache_dir = std::path::Path::new(&state.settings.storybook_cache_dir);
    let asset = service::render::serve_asset(&state.storage, cache_dir, key, build_id, rel_path)
        .await
        .map_err(|e| match e {
            StorybookServeError::NotFound => AppError::NotFound,
            other => AppError::Internal(anyhow::anyhow!("serve storybook asset: {other}")),
        })?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, asset.content_type)
        .header(header::CONTENT_LENGTH, asset.len)
        .header(header::CACHE_CONTROL, CACHE_CONTROL)
        .body(Body::from_stream(asset.stream))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("storybook response: {e}")))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{build_id}/storybook/",
    tag = "Builds",
    summary = "アップロード済み Storybook を開く（index.html）",
    description = "`mode = storybook` のビルドにアップロードされた Storybook を対話的に配信する \
                   エントリポイント。認可はビルド詳細と同じ（テナントメンバー、または \
                   `read:build` の PAT）。バンドル未アップロードや screenshots モードは 404。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Storybook の index.html", content_type = "text/html"),
        CrudErrors,
    )
)]
pub async fn get_storybook_index(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
) -> Result<Response, AppError> {
    serve_storybook(&state, &auth, build_id, "").await
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{build_id}/storybook/{*path}",
    tag = "Builds",
    summary = "アップロード済み Storybook のアセットを配信",
    description = "`iframe.html` や `assets/*.js` など、Storybook が相対パスで読み込む \
                   静的ファイルを配信する。パス解決はトラバーサル安全（`..`・絶対パス・ \
                   シンボリックリンクを拒否）。認可は index と同じ。",
    params(
        ("build_id" = Uuid, Path, description = "ビルドID"),
        ("path" = String, Path, description = "バンドル内の相対パス"),
    ),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "バンドル内のファイル"),
        CrudErrors,
    )
)]
pub async fn get_storybook_asset(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((build_id, path)): Path<(Uuid, String)>,
) -> Result<Response, AppError> {
    serve_storybook(&state, &auth, build_id, &path).await
}
