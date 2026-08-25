//! CI クライアント向けエンドポイント。
//!
//! すべて PAT（`Authorization: Bearer`）で叩かれる想定。CSRF ミドルウェアは
//! Bearer 付きリクエストの Origin 検査をスキップするため、CI からそのまま呼べる。
//!
//! CI は UUID を知らないので、ビルド作成だけ `{tenant_slug}/{project_slug}` で指す。
//! それ以降は返ってきた `build_id` を使う。

use axum::{
    Json,
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
};
use bytes::{Bytes, BytesMut};
use sea_orm::prelude::Uuid;
use serde::Serialize;
use utoipa::ToSchema;
use validator::Validate;

use common::validation::ScreenshotName;

use crate::AppState;
use crate::error::{AppError, ServerError};
use crate::extractors::AuthUser;
use crate::handlers::builds::load_build_with_role;
use crate::openapi::{CrudErrors, SessionAuthErrors};
use entity::{builds::BuildMode, builds::BuildStatus, scopes::Scope, tenant_members::TenantRole};
use payload::builds::*;
use service::builds as build_service;
use service::projects as project_service;
use service::render as render_service;
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

    let mode = payload.mode.unwrap_or_default();

    // Chromium が無いサーバーで storybook ビルドを作らせると、
    // finalize 後に必ずジョブが落ちる。作成時点で断る。
    if mode == BuildMode::Storybook && !state.settings.storybook_render_enabled() {
        return Err(AppError::BadRequestDetail(
            "storybook rendering not configured".into(),
        ));
    }

    let build = build_service::create_build(
        &state.db,
        &project,
        payload.branch,
        payload.commit_sha,
        payload.commit_message,
        payload.pull_request_number,
        mode,
    )
    .await?;

    // CLI が「今回の baseline はどのコミットか」を知って撮り直しを絞れるよう、
    // 作成レスポンスにだけ現時点の baseline のコミット SHA を載せる。ここでは
    // 固定しない——固定は部分撮影の計画が確定した時点（capture plan の添付 /
    // storybook の only_story_ids finalize）で、この値との照合を経て行う。
    let baseline_commit_sha =
        build_service::current_baseline_commit_sha(&state.db, &project, &build.branch).await?;

    let mut response: BuildResponse = build.into();
    response.baseline_commit_sha = baseline_commit_sha;

    Ok((StatusCode::CREATED, Json(response)))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/builds/{build_id}/plan",
    tag = "CI",
    summary = "部分アップロード計画（capture plan）をビルドへ固定する",
    description = "`screenshots` モード専用。撮影を始める**前**に「今回撮る名前」\
                   （`selected_names`）と「現時点で存在する全名前」（`manifest_names`）を\
                   ビルドへ保存し、比較に使う baseline を固定する。以降のアップロードは\
                   `selected_names` 内の名前だけが受理され、finalize は保存された計画と\
                   実アップロードの一致を検証する。計画の起点 `baseline_commit_sha` が\
                   現在の baseline と一致しない場合は 409（再計画が必要）。\
                   スクリーンショットのアップロード後には添付できない（409）。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    request_body = AttachCapturePlanRequest,
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "計画を固定したビルド（baseline_commit_sha は固定値）", body = BuildResponse),
        (status = 400, description = "リストが不正（名前は空でなく前後空白なし・255 バイト以内） / selected が manifest の部分集合でない / 新規名（manifest にあり baseline に無い）が selected から漏れている / baseline と manifest の名前が 1 件も重ならない（命名規則の不一致とみなす） / storybook モード", body = ServerError),
        (status = 409, description = "pending でない / 計画添付済み / アップロード済み / baseline が移動した / baseline が無い", body = ServerError),
        CrudErrors,
    )
)]
pub async fn attach_capture_plan(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
    Json(payload): Json<AttachCapturePlanRequest>,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_scope(Scope::WriteBuild)?;
    // 名前規則（ScreenshotName——アップロード・finalize と同一）での検証と型変換。
    let (selected_names, manifest_names) =
        payload.parse_lists().map_err(AppError::BadRequestDetail)?;

    let (build, project) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;

    let build = build_service::attach_capture_plan(
        &state.db,
        build,
        &project,
        selected_names,
        manifest_names,
        &payload.baseline_commit_sha,
    )
    .await?;

    // 固定した baseline のコミット SHA を返し、クライアントが照合できるようにする。
    let baseline_commit_sha = build_service::pinned_baseline_commit_sha(&state.db, &build).await?;
    let mut response: BuildResponse = build.into();
    response.baseline_commit_sha = baseline_commit_sha;
    Ok(Json(response))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/builds/{build_id}/screenshots",
    tag = "CI",
    summary = "スクリーンショットをアップロードする",
    description = "multipart/form-data。`name` フィールドを `file` より**前**に送ること。\
                   PNG のみ受け付ける（最大 25MB / 10000x10000）。\
                   `pending` 以外のビルドへのアップロードは 409、同名の重複も 409。\
                   capture plan が固定されたビルドでは、計画の `selected_names` に\
                   無い名前のアップロードは 400 で拒否される。",
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

    // 事前検査（バイト列を読む前の速い失敗）。正とするのは store_ci_screenshot が
    // build 行ロックの中で行う再検査で、ここは並行変更に対して権威を持たない。
    // finalize 後のアップロードは受け付けない。
    if build.status != BuildStatus::Pending {
        return Err(AppError::Conflict);
    }
    // storybook モードのビルドはサーバー側が撮る。CI からの直アップロードは受け付けない。
    if build.mode != BuildMode::Screenshots {
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
    // 名前規則（ScreenshotName——capture plan・finalize と同一）。空白付きを黙って
    // trim すると、計画に載せた名前と保存された名前がずれて突き合わせが壊れる。
    let name = ScreenshotName::parse(name)
        .map_err(|e| AppError::BadRequestDetail(format!("name: {e}")))?;
    let file = file.ok_or_else(|| AppError::BadRequestDetail("file field is required".into()))?;

    // 状態・モード・capture plan（計画外の名前は 400）・重複の権威ある検査は、
    // build 行ロックの中で DB 挿入と同時に行う。計画添付との並行競合で
    // 「添付前の検査を通った計画外ショット」が紛れ込むのを防ぐ。
    let screenshot = screenshot_service::store_ci_screenshot(
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
    path = "/builds/{build_id}/storybook",
    tag = "CI",
    summary = "ビルド済み Storybook（storybook-static の zip）をアップロードする",
    description = "multipart/form-data の `file` フィールドに zip を送る（最大 200MB）。\
                   `mode = storybook` かつ `pending` のビルドにだけ許可され、\
                   1 ビルドにつき 1 本まで（再アップロードは 409）。\
                   アップロード後 `POST /v1/ci/builds/{build_id}/finalize` を呼ぶと\
                   サーバー側でヘッドレス Chromium が全ストーリーを撮影する。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "保存されたバンドル", body = StorybookBundleResponse),
        (status = 400, description = "zip ではない / フィールドが足りません", body = ServerError),
        (status = 409, description = "storybook モードでない・pending でない・既にアップロード済み", body = ServerError),
        (status = 413, description = "ファイルが大きすぎます", body = ServerError),
        CrudErrors,
    )
)]
pub async fn upload_storybook_bundle(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<StorybookBundleResponse>), AppError> {
    auth.require_scope(Scope::WriteBuild)?;
    let (build, project) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;

    // モード・状態・重複は、バイト列を読む前に弾く。
    if build.mode != BuildMode::Storybook || build.status != BuildStatus::Pending {
        return Err(AppError::Conflict);
    }
    if build.storybook_key.is_some() {
        return Err(AppError::Conflict);
    }

    let mut file: Option<Bytes> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequestDetail(format!("invalid multipart body: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        // 上限を超えた時点で打ち切る（全部読んでから弾かない）。
        let mut buf = BytesMut::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| AppError::BadRequestDetail(format!("invalid file field: {e}")))?
        {
            if buf.len() + chunk.len() > render_service::MAX_BUNDLE_BYTES {
                return Err(AppError::ContentTooLarge);
            }
            buf.extend_from_slice(&chunk);
        }
        file = Some(buf.freeze());
    }

    let file = file.ok_or_else(|| AppError::BadRequestDetail("file field is required".into()))?;
    render_service::validate_zip(&file)?;
    let size_bytes = file.len() as u64;

    let key = render_service::storybook_key(project.tenant_id, project.id, build.id);
    render_service::upload_bundle(&state.storage, &key, file)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("upload storybook bundle: {e}")))?;

    build_service::attach_storybook_bundle(&state.db, build, key).await?;

    Ok((
        StatusCode::CREATED,
        Json(StorybookBundleResponse {
            build_id,
            size_bytes,
        }),
    ))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/builds/{build_id}/finalize",
    tag = "CI",
    summary = "アップロードを締めてジョブを投入する",
    description = "どちらのモードも `pending → queued` に遷移してジョブを投入する。\
                   worker が取得すると `screenshots` は `processing`、`storybook` は \
                   `rendering` に進み、レンダリング後は自動で `processing` に繋がる。\
                   以降は `GET /v1/ci/builds/{build_id}` をポーリングして結果を待つ。\
                   ボディは任意。`storybook` モードで `only_story_ids` を渡すと、\
                   そのストーリーだけを撮影し残りは baseline を流用する（TurboSnap 相当。\
                   このとき `expected_baseline_commit_sha` は必須で、現在の baseline と\
                   照合してから比較対象として固定する）。\
                   `screenshots` モードの部分アップロードは、事前に\
                   `POST /v1/ci/builds/{build_id}/plan` で固定された capture plan と\
                   実際のアップロードの一致を検証する。`captured_names` は保存済み計画との\
                   任意のクロスチェックで、計画なしのビルドに渡すと 400。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    request_body(content = FinalizeBuildRequest, description = "任意。省略・空ボディ可"),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "queued に遷移したビルド", body = BuildResponse),
        (status = 400, description = "storybook バンドルが未アップロード / リストが不正 / モードとフィールドの組合せが不正 / captured_names とアップロードの不一致 / expected_baseline_commit_sha と固定済み baseline の不一致（screenshots モードのクロスチェック）", body = ServerError),
        (status = 409, description = "既に finalize 済みです / baseline が計画後に動いた（expected_baseline_commit_sha と現在の baseline の不一致。現在の baseline へ再計画すれば解消）", body = ServerError),
        CrudErrors,
    )
)]
pub async fn finalize_build(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
    // ボディは任意。Content-Type 無しの空 POST も従来どおり受けたいので、
    // `Json<T>` ではなく生の `Bytes` で受けて自前でパースする
    // （`Json<T>` は Content-Type: application/json を要求して弾いてしまう）。
    body: Bytes,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_scope(Scope::WriteBuild)?;

    // 空ボディ = 全撮影（従来どおり）。中身があるときだけ JSON として解釈する。
    let payload: FinalizeBuildRequest = if body.is_empty() {
        FinalizeBuildRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| AppError::BadRequestDetail(format!("invalid finalize body: {e}")))?
    };
    payload
        .validate_story_ids()
        .map_err(AppError::BadRequestDetail)?;
    // captured_names はスクリーンショット名なので、story ID の規則ではなく
    // 名前規則（ScreenshotName——capture plan・アップロードと同一）で検証する。
    let captured_names = payload
        .parse_captured_names()
        .map_err(AppError::BadRequestDetail)?;

    let (build, project) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;

    let build = match build.mode {
        BuildMode::Screenshots => {
            // screenshots モードはサーバーがレンダリングしないため、ストーリー ID を
            // スクリーンショット名へ写像できず only_story_ids は成立しない。
            // 部分アップロードは事前に POST /plan で固定した capture plan で表す。
            if payload.only_story_ids.is_some() {
                return Err(AppError::BadRequestDetail(
                    "only_story_ids is not supported for screenshots-mode builds; \
                     attach a capture plan via POST /v1/ci/builds/{id}/plan instead"
                        .into(),
                ));
            }
            // クライアントが計画に使った baseline と、計画添付時に固定された
            // baseline の照合。計画なしのビルドには固定値が無いので、照合の
            // しようがない（黙って通すと照合した気になるだけなので 400）。
            if let Some(expected) = &payload.expected_baseline_commit_sha {
                let pinned = build_service::pinned_baseline_commit_sha(&state.db, &build).await?;
                match pinned {
                    // 固定 SHA を解決できない理由は 2 つあり、文言を分ける:
                    // baseline 自体が固定されていない（計画なし）のか、固定は
                    // されているが昇格元ビルドが削除されて SHA を辿れないのか。
                    // 後者に「requires a capture plan」を返すと、計画も pin も
                    // あるのに無いと言われて原因に辿れない。
                    None if build.baseline_id.is_some() => {
                        return Err(AppError::BadRequestDetail(
                            "expected_baseline_commit_sha cannot be verified: a baseline \
                             is pinned to this build, but the build it was promoted from \
                             (which carries the commit SHA) no longer exists. re-create \
                             the build and attach a fresh capture plan, or finalize \
                             without expected_baseline_commit_sha"
                                .into(),
                        ));
                    }
                    None => {
                        return Err(AppError::BadRequestDetail(
                            "expected_baseline_commit_sha requires a capture plan: \
                             no baseline is pinned to this build, so there is nothing \
                             to verify against"
                                .into(),
                        ));
                    }
                    Some(pinned) if pinned != *expected => {
                        return Err(AppError::BadRequestDetail(format!(
                            "expected_baseline_commit_sha does not match the baseline \
                             pinned to this build (expected {expected}, pinned {pinned})"
                        )));
                    }
                    Some(_) => {}
                }
            }
            let build =
                build_service::finalize_screenshots(&state.db, build, captured_names).await?;
            job::compare_build::enqueue(
                &state.compare_build_storage,
                job::CompareBuildJob { build_id: build.id },
            )
            .await
            .map_err(AppError::Internal)?;
            build
        }
        BuildMode::Storybook => {
            // storybook モードはサーバーが撮るので「CI が撮った名前」の宣言は成立しない。
            if payload.captured_names.is_some() {
                return Err(AppError::BadRequestDetail(
                    "captured_names is not supported for storybook-mode builds; \
                     use only_story_ids to narrow the capture set"
                        .into(),
                ));
            }
            // 部分レンダリングは、計画の起点 baseline の照合と固定を finalize の
            // 行ロック 1 トランザクション内（finalize_storybook）で行う。
            // 全撮影（only_story_ids 無し）は固定しない——比較ジョブが比較時点の
            // 最新 baseline を解決する従来動作のまま。
            let pin_expected = if payload.only_story_ids.is_some() {
                let Some(expected) = &payload.expected_baseline_commit_sha else {
                    return Err(AppError::BadRequestDetail(
                        "only_story_ids requires expected_baseline_commit_sha so the \
                         baseline the plan was computed against can be verified and \
                         pinned before any reuse happens"
                            .into(),
                    ));
                };
                Some(expected.clone())
            } else {
                if let Some(expected) = &payload.expected_baseline_commit_sha {
                    // 全撮影でも、渡された起点が現在の baseline とずれていれば断る
                    // （固定はしない読み取り検査なので、finalize の行ロックの外でよい）。
                    // 「baseline が動いた（再計画で解消）」は plan 添付と同じ 409。
                    let current = build_service::current_baseline_commit_sha(
                        &state.db,
                        &project,
                        &build.branch,
                    )
                    .await?;
                    if current.as_deref() != Some(expected.as_str()) {
                        return Err(AppError::ConflictDetail(format!(
                            "expected_baseline_commit_sha does not match the current baseline \
                             (expected {expected}, current {})",
                            current.as_deref().unwrap_or("none")
                        )));
                    }
                }
                None
            };
            let build =
                build_service::finalize_storybook(&state.db, build, &project, pin_expected).await?;
            job::render_build::enqueue(
                &state.render_build_storage,
                job::RenderBuildJob {
                    build_id: build.id,
                    only_story_ids: payload.only_story_ids,
                },
            )
            .await
            .map_err(AppError::Internal)?;
            build
        }
    };

    // `queued` を GitHub の pending ステータスとして先に見せる。
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

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/builds/{build_id}/logs",
    tag = "CI",
    summary = "ビルドの進捗ログを取得する（CI の追尾用）",
    description = "`read:build` スコープが必要。`after` カーソルで増分取得する。\
                   CLI の `--wait` がポーリング中に新着行を stdout へ流すために使う。",
    params(
        ("build_id" = Uuid, Path, description = "ビルドID"),
        BuildLogsQuery,
    ),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "進捗ログの行", body = BuildLogsResponse),
        CrudErrors,
    )
)]
pub async fn get_build_logs(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
    Query(query): Query<BuildLogsQuery>,
) -> Result<Json<BuildLogsResponse>, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let (build, _) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;

    let after = query.after.unwrap_or(0);
    let entries = service::build_logs::list_after(
        &state.db,
        build.id,
        after,
        service::build_logs::MAX_LIST_LIMIT,
    )
    .await?;
    let last_id = service::build_logs::resolve_last_id(after, &entries);

    Ok(Json(BuildLogsResponse {
        entries: entries.into_iter().map(Into::into).collect(),
        last_id,
    }))
}
