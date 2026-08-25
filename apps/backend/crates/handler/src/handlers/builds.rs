//! ビルドの HTTP ハンドラ（UI 側）。
//!
//! すべてのアクセスは `build → project → tenant メンバーシップ` まで辿って検査する。
//! 非メンバーには「存在しない」と「権限がない」を区別させないため 403 に揃える
//! （`handlers::projects::load_project_with_role` と同じ方針）。

use axum::{
    Json,
    extract::{Path, Query, State},
};
use sea_orm::prelude::Uuid;

use crate::AppState;
use crate::error::{AppError, ServerError};
use crate::extractors::AuthUser;
use crate::openapi::CrudErrors;
use entity::{
    baseline_entries, builds, builds::BuildMode, comparisons, projects, scopes::Scope, screenshots,
    tenant_members::TenantRole,
};
use payload::builds::*;
use payload::comparisons::*;
use service::approval::ApproveOptions;
use service::build_logs as log_service;
use service::builds as build_service;
use service::comparisons as comparison_service;
use service::projects as project_service;
use service::tenants as tenant_service;

// ── アクセス解決ヘルパー ─────────────────────────────────────────────────

/// プロジェクトを読み込み、所有テナントに対する `min_role` を要求する。
pub(crate) async fn load_project_with_role(
    state: &AppState,
    project_id: Uuid,
    user_id: Uuid,
    min_role: TenantRole,
) -> Result<projects::Model, AppError> {
    let project = project_service::get_project(&state.db, project_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    tenant_service::require_role(&state.db, project.tenant_id, user_id, min_role).await?;
    Ok(project)
}

/// ビルドと所有プロジェクトを解決し、テナントのロールを検査する。
pub(crate) async fn load_build_with_role(
    state: &AppState,
    build_id: Uuid,
    user_id: Uuid,
    min_role: TenantRole,
) -> Result<(builds::Model, projects::Model), AppError> {
    let build = build_service::get_build(&state.db, build_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    let project = load_project_with_role(state, build.project_id, user_id, min_role).await?;
    Ok((build, project))
}

/// スクリーンショット → ビルド → プロジェクト → テナントまで辿る。
pub(crate) async fn load_screenshot_with_role(
    state: &AppState,
    screenshot_id: Uuid,
    user_id: Uuid,
    min_role: TenantRole,
) -> Result<(screenshots::Model, projects::Model), AppError> {
    let shot = service::screenshots::get_screenshot(&state.db, screenshot_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    let (_, project) = load_build_with_role(state, shot.build_id, user_id, min_role).await?;
    Ok((shot, project))
}

/// baseline エントリ → baseline → プロジェクト → テナントまで辿る。
pub(crate) async fn load_baseline_entry_with_role(
    state: &AppState,
    entry_id: Uuid,
    user_id: Uuid,
    min_role: TenantRole,
) -> Result<(baseline_entries::Model, projects::Model), AppError> {
    let entry = service::baselines::get_entry(&state.db, entry_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    let baseline = service::baselines::get_baseline(&state.db, entry.baseline_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    let project = load_project_with_role(state, baseline.project_id, user_id, min_role).await?;
    Ok((entry, project))
}

/// 比較 → ビルド → プロジェクト → テナントまで辿る。
pub(crate) async fn load_comparison_with_role(
    state: &AppState,
    comparison_id: Uuid,
    user_id: Uuid,
    min_role: TenantRole,
) -> Result<(comparisons::Model, builds::Model, projects::Model), AppError> {
    let comparison = comparison_service::get_comparison(&state.db, comparison_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound => AppError::Forbidden,
            other => other,
        })?;
    let (build, project) =
        load_build_with_role(state, comparison.build_id, user_id, min_role).await?;
    Ok((comparison, build, project))
}

// ── /v1/projects/{project_id}/builds ────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{project_id}/builds",
    tag = "Builds",
    summary = "プロジェクトのビルド一覧",
    description = "ビルド番号の降順。テナントのメンバーであること。PAT は `read:build` で参照できる。",
    params(
        ("project_id" = Uuid, Path, description = "プロジェクトID"),
        BuildListQuery,
    ),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "ビルド一覧", body = BuildListResponse),
        CrudErrors,
    )
)]
pub async fn list_builds(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(project_id): Path<Uuid>,
    Query(query): Query<BuildListQuery>,
) -> Result<Json<BuildListResponse>, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let project =
        load_project_with_role(&state, project_id, auth.user_id, TenantRole::Member).await?;

    let limit = query.limit.unwrap_or(build_service::DEFAULT_LIST_LIMIT);
    let offset = query.offset.unwrap_or(0);

    let list = build_service::list_builds(&state.db, project.id, limit, offset).await?;
    let baseline_sources = build_service::baseline_sources_for_builds(&state.db, &list).await?;
    let total = build_service::count_builds(&state.db, project.id).await?;

    let builds = list
        .into_iter()
        .map(|model| {
            let source = baseline_sources.get(&model.id);
            let mut response: BuildResponse = model.into();
            response.baseline_source = source.map(|source| BuildBaselineSourceResponse {
                branch: source.branch.clone(),
                build_id: source.source_build_id,
                build_number: source.source_build_number,
            });
            response
        })
        .collect();

    Ok(Json(BuildListResponse { builds, total }))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{project_id}/builds/{number}",
    tag = "Builds",
    summary = "ビルド番号でビルドを取得",
    description = "プロジェクト内で連番のビルド番号から引く。UI の URL が番号ベースなので \
                   一覧を舐めずに 1 件だけ取れるようにしてある。認可は ID 直参照と同じ。",
    params(
        ("project_id" = Uuid, Path, description = "プロジェクトID"),
        ("number" = i64, Path, description = "プロジェクト内のビルド番号"),
    ),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "ビルド情報", body = BuildResponse),
        CrudErrors,
    )
)]
pub async fn get_build_by_number(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((project_id, number)): Path<(Uuid, i64)>,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let project =
        load_project_with_role(&state, project_id, auth.user_id, TenantRole::Member).await?;
    let build = build_service::get_build_by_number(&state.db, project.id, number).await?;
    Ok(Json(build.into()))
}

// ── /v1/builds/{build_id} ───────────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{build_id}",
    tag = "Builds",
    summary = "ビルドを取得",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "ビルド情報", body = BuildResponse),
        CrudErrors,
    )
)]
pub async fn get_build(
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
    path = "/{build_id}/logs",
    tag = "Builds",
    summary = "ビルドの進捗ログ（増分取得）",
    description = "render / compare ジョブが追記した進捗ログを `after` カーソルで増分取得する。\
                   テナントのメンバーであること。進行中は UI がポーリングする。",
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
    let entries =
        service::build_logs::list_after(&state.db, build.id, after, log_service::MAX_LIST_LIMIT)
            .await?;
    let last_id = log_service::resolve_last_id(after, &entries);

    Ok(Json(BuildLogsResponse {
        entries: entries.into_iter().map(Into::into).collect(),
        last_id,
    }))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/{build_id}/comparisons",
    tag = "Builds",
    summary = "ビルドの比較結果一覧",
    description = "スクリーンショット名の昇順。レビュー UI が使う。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "比較結果一覧", body = ComparisonListResponse),
        CrudErrors,
    )
)]
pub async fn list_comparisons(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
) -> Result<Json<ComparisonListResponse>, AppError> {
    auth.require_scope(Scope::ReadBuild)?;
    let (build, _) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Member).await?;

    let list = comparison_service::list_for_build(&state.db, build.id).await?;
    let total = list.len() as u64;
    Ok(Json(ComparisonListResponse {
        comparisons: list.into_iter().map(Into::into).collect(),
        total,
    }))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/{build_id}/approve",
    tag = "Builds",
    summary = "ビルドを承認して baseline に昇格",
    description = "admin 以上が必要。承認するとこのビルドの全スクリーンショットが \
                   `(project, branch)` の新しい baseline になる。\
                   未レビューの比較が残っている場合は 409。`force: true` で一括承認できる \
                   （`removed` と `failed` は含まない。各専用フラグで明示する）。\
                   却下された比較が残っている場合、比較後に baseline が進んでいる場合、\
                   承認されていない story の欠落がある場合も 409。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    request_body = ApproveBuildRequest,
    responses(
        (status = 200, description = "承認後のビルド", body = BuildResponse),
        (status = 409, description = "承認できない状態、または未レビューの比較が残っています", body = ServerError),
        CrudErrors,
    )
)]
pub async fn approve_build(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
    body: Option<Json<ApproveBuildRequest>>,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_session()?;
    let (build, _) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Admin).await?;

    let options = body
        .map(|Json(b)| ApproveOptions {
            force: b.force,
            accept_removals: b.accept_removals,
            accept_failures: b.accept_failures,
            accept_revert: b.accept_revert,
        })
        .unwrap_or_default();
    let approved =
        build_service::approve_build(&state.db, &state.storage, build, auth.user_id, options)
            .await?;

    // レビュー結果を PR に反映する。job を handler に依存させないため、
    // 投入はサービス呼び出しが成功したあとのここで行う。
    job::github_status::enqueue_best_effort(&state.github_status_storage, approved.id).await;

    Ok(Json(approved.into()))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/{build_id}/reject",
    tag = "Builds",
    summary = "ビルドを却下",
    description = "admin 以上が必要。baseline は更新されず、未レビューの比較は rejected になる。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    responses(
        (status = 200, description = "却下後のビルド", body = BuildResponse),
        (status = 409, description = "却下できない状態です", body = ServerError),
        CrudErrors,
    )
)]
pub async fn reject_build(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_session()?;
    let (build, _) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Admin).await?;
    let rejected = build_service::reject_build(&state.db, build, auth.user_id).await?;

    job::github_status::enqueue_best_effort(&state.github_status_storage, rejected.id).await;

    Ok(Json(rejected.into()))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/{build_id}/retry",
    tag = "Builds",
    summary = "失敗したビルドを再実行",
    description = "admin 以上が必要。`failed` のビルドだけを再実行できる。\
                   storybook モードはアップロード済みバンドルの再レンダリングから、\
                   screenshots モードはアップロード済みスクリーンショットの比較から \
                   やり直す。`failed` 以外、またはバンドル未アップロードの failed は 409。",
    params(("build_id" = Uuid, Path, description = "ビルドID")),
    responses(
        (status = 200, description = "再実行を開始したビルド", body = BuildResponse),
        (status = 409, description = "再実行できない状態です", body = ServerError),
        CrudErrors,
    )
)]
pub async fn retry_build(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(build_id): Path<Uuid>,
) -> Result<Json<BuildResponse>, AppError> {
    auth.require_session()?;
    let (build, _) =
        load_build_with_role(&state, build_id, auth.user_id, TenantRole::Admin).await?;

    // 外部 runner も内蔵 worker も無い構成では queued に戻しても永久に進まない。
    // 設定はプロセス起動中に変わらないため、状態遷移より先に拒否して failed を保つ。
    if build.mode == BuildMode::Storybook && !state.settings.storybook_render_enabled() {
        return Err(AppError::ConflictDetail(
            "Storybook rendering is disabled; configure an internal worker or external runner before retrying this build"
                .into(),
        ));
    }

    // 状態・バンドル検査は retry_failed が行ロック下で取り直した行に対して行う。
    let (build, target) = build_service::retry_failed(&state.db, build.id).await?;

    // 進捗ログに区切りを残す——前回の失敗ログの直後から新しい実行のログが
    // 続くので、どこからが再実行か読み手に分かるようにする。
    log_service::append(
        &state.db,
        build.id,
        log_service::LogLevel::Info,
        "build retry requested; previous results were discarded",
    )
    .await?;

    // ジョブ投入は遷移が成功したあとのここで行う（finalize と同じ分担。
    // job を handler に依存させない）。各ジョブは開始時に前回の途中結果を
    // 自分で捨てるので、ここでの掃除は不要。
    match target {
        build_service::RetryTarget::Render => {
            job::render_build::enqueue(
                &state.render_build_storage,
                job::RenderBuildJob {
                    build_id: build.id,
                    only_story_ids: None,
                },
            )
            .await
            .map_err(AppError::Internal)?;
        }
        build_service::RetryTarget::Compare => {
            job::compare_build::enqueue(
                &state.compare_build_storage,
                job::CompareBuildJob { build_id: build.id },
            )
            .await
            .map_err(AppError::Internal)?;
        }
    }

    // queued を GitHub の pending ステータスとして見せる
    // （finalize と同じ）。連携が無ければジョブ側が何もせず終わる。
    job::github_status::enqueue_best_effort(&state.github_status_storage, build.id).await;

    Ok(Json(build.into()))
}
