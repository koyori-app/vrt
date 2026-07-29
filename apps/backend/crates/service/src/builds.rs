//! ビルドのライフサイクル管理。
//!
//! 状態遷移は [`transition`] に一本化し、不正な遷移は必ず [`AppError::Conflict`] にする。
//! 承認 ([`approve_build`]) はプロジェクト行を `SELECT ... FOR UPDATE` で直列化してから
//! baseline を作るため、同一プロジェクトの並行承認でも baseline が競合しない。

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    DbBackend, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
    prelude::Uuid,
};

use common::db::with_transaction;
use common::error::AppError;
use entity::{
    baseline_entries, baselines, builds, builds::BuildMode, builds::BuildStatus, comparisons,
    comparisons::ComparisonStatus, comparisons::ReviewStatus, projects, screenshots,
};

use crate::storage::StorageBackend;

/// ビルド一覧のデフォルト件数。
pub const DEFAULT_LIST_LIMIT: u64 = 30;
/// ビルド一覧の最大件数。
pub const MAX_LIST_LIMIT: u64 = 100;

/// 集計済みの比較結果カウント。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BuildCounts {
    pub total: i32,
    pub changed: i32,
    pub added: i32,
    pub removed: i32,
    pub unchanged: i32,
}

impl BuildCounts {
    /// 差分（changed / added / removed）が 1 件でもあるか。
    pub fn has_differences(self) -> bool {
        self.changed > 0 || self.added > 0 || self.removed > 0
    }
}

/// プロジェクト内で欠番のないビルド番号を払い出す。
///
/// task の `project_task_counters` と同じ upsert パターン。
/// `INSERT ... ON CONFLICT DO UPDATE SET counter = counter + 1 RETURNING counter` は
/// 1 ステートメントで行ロックまで完結するため、並行 INSERT でも番号が飛ばない。
pub async fn next_build_number<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
) -> Result<i64, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO project_build_counters (project_id, counter)
            VALUES ($1, 1)
            ON CONFLICT (project_id) DO UPDATE
                SET counter = project_build_counters.counter + 1
            RETURNING counter
            "#,
            vec![project_id.into()],
        ))
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("build counter upsert returned no row"))
        })?;

    Ok(row.try_get_by_index::<i64>(0)?)
}

/// 新しいビルドを `pending` で作成する。
///
/// `mode` は入力形式。`storybook` のときは screenshot のアップロードを受け付けず、
/// 代わりに `POST /v1/ci/builds/{id}/storybook` でバンドルを受け取る。
#[allow(clippy::too_many_arguments)]
pub async fn create_build<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
    branch: String,
    commit_sha: String,
    commit_message: Option<String>,
    pull_request_number: Option<i32>,
    mode: BuildMode,
) -> Result<builds::Model, AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::BadRequestDetail("branch is required".into()));
    }
    if commit_sha.trim().is_empty() {
        return Err(AppError::BadRequestDetail("commit_sha is required".into()));
    }

    let number = next_build_number(db, project_id).await?;

    Ok(builds::ActiveModel {
        id: Set(Uuid::new_v4()),
        project_id: Set(project_id),
        number: Set(number),
        branch: Set(branch),
        commit_sha: Set(commit_sha),
        commit_message: Set(commit_message),
        pull_request_number: Set(pull_request_number),
        status: Set(BuildStatus::Pending),
        mode: Set(mode),
        storybook_key: Set(None),
        baseline_id: Set(None),
        total_count: Set(0),
        changed_count: Set(0),
        added_count: Set(0),
        removed_count: Set(0),
        unchanged_count: Set(0),
        error_message: Set(None),
        approved_by: Set(None),
        approved_at: Set(None),
        created_at: Set(Utc::now().fixed_offset()),
        completed_at: Set(None),
    }
    .insert(db)
    .await?)
}

/// ビルドを ID で取得する。
pub async fn get_build<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
) -> Result<builds::Model, AppError> {
    builds::Entity::find_by_id(build_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// プロジェクトのビルド一覧（新しい順）。
pub async fn list_builds<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
    limit: u64,
    offset: u64,
) -> Result<Vec<builds::Model>, AppError> {
    Ok(builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .order_by_desc(builds::Column::Number)
        .limit(limit.clamp(1, MAX_LIST_LIMIT))
        .offset(offset)
        .all(db)
        .await?)
}

/// プロジェクト内のビルド番号でビルドを取得する。
///
/// `(project_id, number)` は一意。UI の `/builds/{number}` 表示が一覧を舐めずに済むように使う。
pub async fn get_build_by_number<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
    number: i64,
) -> Result<builds::Model, AppError> {
    builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .filter(builds::Column::Number.eq(number))
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// プロジェクトのビルド総数（ページネーション用）。
pub async fn count_builds<C: ConnectionTrait>(db: &C, project_id: Uuid) -> Result<u64, AppError> {
    Ok(builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .count(db)
        .await?)
}

/// 状態遷移。許可されていない遷移は [`AppError::Conflict`]。
///
/// パイプラインが完走した状態（passed / changes_detected / failed）に入るときに
/// `completed_at` を打つ。承認・却下では触らない
/// （セマンティクスは [`BuildStatus::completes_pipeline`] 参照）。
pub async fn transition<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    to: BuildStatus,
) -> Result<builds::Model, AppError> {
    if !build.status.can_transition_to(to) {
        return Err(AppError::Conflict);
    }

    let mut active: builds::ActiveModel = build.into();
    active.status = Set(to);
    if to.completes_pipeline() {
        active.completed_at = Set(Some(Utc::now().fixed_offset()));
    }
    Ok(active.update(db).await?)
}

/// finalize: `pending → processing`。ジョブ投入は呼び出し側（ハンドラ）が行う。
pub async fn finalize<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
) -> Result<builds::Model, AppError> {
    transition(db, build, BuildStatus::Processing).await
}

/// storybook モードの finalize: `pending → rendering`。
///
/// バンドルが未アップロードなら 409（`RenderBuildJob` が拾うものが無いため）。
/// ジョブ投入は呼び出し側（ハンドラ）が行う。
pub async fn finalize_storybook<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
) -> Result<builds::Model, AppError> {
    if build.storybook_key.is_none() {
        return Err(AppError::BadRequestDetail(
            "storybook bundle has not been uploaded for this build".into(),
        ));
    }
    transition(db, build, BuildStatus::Rendering).await
}

/// アップロードされた Storybook バンドルのストレージキーを記録する。
///
/// 1 ビルドにつき 1 本だけ。既に記録済みなら [`AppError::Conflict`]。
pub async fn attach_storybook_bundle<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    key: String,
) -> Result<builds::Model, AppError> {
    if build.mode != BuildMode::Storybook {
        return Err(AppError::Conflict);
    }
    if build.status != BuildStatus::Pending {
        return Err(AppError::Conflict);
    }
    if build.storybook_key.is_some() {
        return Err(AppError::Conflict);
    }

    let mut active: builds::ActiveModel = build.into();
    active.storybook_key = Set(Some(key));
    Ok(active.update(db).await?)
}

/// 比較結果のカウントを集計して build に書き戻す。
pub async fn apply_counts<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    counts: BuildCounts,
    baseline_id: Option<Uuid>,
) -> Result<builds::Model, AppError> {
    let mut active: builds::ActiveModel = build.into();
    active.total_count = Set(counts.total);
    active.changed_count = Set(counts.changed);
    active.added_count = Set(counts.added);
    active.removed_count = Set(counts.removed);
    active.unchanged_count = Set(counts.unchanged);
    active.baseline_id = Set(baseline_id);
    Ok(active.update(db).await?)
}

/// ジョブが回復不能なエラーで落ちたときの終着点。
pub async fn mark_failed<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    message: String,
) -> Result<builds::Model, AppError> {
    // 既に終端状態なら何もしない（リトライ時の二重書き込み防止）。
    if build.status.is_terminal() {
        return Ok(build);
    }
    let mut active: builds::ActiveModel = build.into();
    active.status = Set(BuildStatus::Failed);
    active.error_message = Set(Some(message));
    active.completed_at = Set(Some(Utc::now().fixed_offset()));
    Ok(active.update(db).await?)
}

/// レビュー待ち（`review_status = pending` かつ人手判断が要る）の比較件数。
pub async fn pending_review_count<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
) -> Result<u64, AppError> {
    Ok(comparisons::Entity::find()
        .filter(comparisons::Column::BuildId.eq(build_id))
        .filter(comparisons::Column::ReviewStatus.eq(ReviewStatus::Pending))
        .filter(comparisons::Column::Status.is_not_in([ComparisonStatus::Unchanged]))
        .count(db)
        .await?)
}

/// ビルドを承認し、そのビルドの全スクリーンショットを新しい baseline に昇格する。
///
/// - `force == false` のときはレビュー待ちの比較が残っていると [`AppError::Conflict`]
/// - `force == true` は「一括承認」用。未レビューの比較もまとめて approved にする
///
/// トランザクション内でプロジェクト行を `SELECT ... FOR UPDATE` してから baseline を作る。
/// これにより同一プロジェクトの並行承認が直列化され、`baselines` の
/// `(project_id, branch, created_at DESC)` 先頭が確定する。
pub async fn approve_build(
    db: &DatabaseConnection,
    build: builds::Model,
    reviewer_id: Uuid,
    force: bool,
) -> Result<builds::Model, AppError> {
    if !build.status.can_transition_to(BuildStatus::Approved) {
        return Err(AppError::Conflict);
    }

    with_transaction(db, move |txn| {
        Box::pin(async move {
            // プロジェクト行ロックで並行承認を直列化する。
            projects::Entity::find_by_id(build.project_id)
                .lock_exclusive()
                .one(txn)
                .await?
                .ok_or(AppError::NotFound)?;

            // ロック取得までに他の承認が走っている可能性があるため状態を読み直す。
            let build = get_build(txn, build.id).await?;
            if !build.status.can_transition_to(BuildStatus::Approved) {
                return Err(AppError::Conflict);
            }

            let pending = pending_review_count(txn, build.id).await?;
            if pending > 0 {
                if !force {
                    return Err(AppError::Conflict);
                }
                approve_all_pending(txn, build.id, reviewer_id).await?;
            }

            let now = Utc::now().fixed_offset();

            // このビルドの全スクリーンショットを新 baseline のエントリにする。
            let shots = screenshots::Entity::find()
                .filter(screenshots::Column::BuildId.eq(build.id))
                .order_by_asc(screenshots::Column::Name)
                .all(txn)
                .await?;

            let baseline = baselines::ActiveModel {
                id: Set(Uuid::new_v4()),
                project_id: Set(build.project_id),
                branch: Set(build.branch.clone()),
                source_build_id: Set(Some(build.id)),
                created_at: Set(now),
            }
            .insert(txn)
            .await?;

            for shot in shots {
                baseline_entries::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    baseline_id: Set(baseline.id),
                    name: Set(shot.name),
                    storage_key: Set(shot.storage_key),
                    width: Set(shot.width),
                    height: Set(shot.height),
                }
                .insert(txn)
                .await?;
            }

            // 承認時刻は `approved_at`。`completed_at`（自動処理が終わった時刻）は
            // 比較フェーズで打ったものを保持する。未設定の古い行だけ埋める。
            let backfill = build.completed_at.is_none();
            let mut active: builds::ActiveModel = build.into();
            active.status = Set(BuildStatus::Approved);
            active.approved_by = Set(Some(reviewer_id));
            active.approved_at = Set(Some(now));
            if backfill {
                active.completed_at = Set(Some(now));
            }
            Ok(active.update(txn).await?)
        })
    })
    .await
}

/// 未レビューの比較をまとめて approved にする（一括承認）。
async fn approve_all_pending<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
    reviewer_id: Uuid,
) -> Result<(), AppError> {
    let now = Utc::now().fixed_offset();
    comparisons::Entity::update_many()
        .col_expr(
            comparisons::Column::ReviewStatus,
            sea_orm::sea_query::Expr::value(ReviewStatus::Approved),
        )
        .col_expr(
            comparisons::Column::ReviewedBy,
            sea_orm::sea_query::Expr::value(reviewer_id),
        )
        .col_expr(
            comparisons::Column::ReviewedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            comparisons::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(comparisons::Column::BuildId.eq(build_id))
        .filter(comparisons::Column::ReviewStatus.eq(ReviewStatus::Pending))
        .exec(db)
        .await?;
    Ok(())
}

/// ビルドを却下する（baseline は更新しない）。
pub async fn reject_build<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    reviewer_id: Uuid,
) -> Result<builds::Model, AppError> {
    if !build.status.can_transition_to(BuildStatus::Rejected) {
        return Err(AppError::Conflict);
    }
    let now = Utc::now().fixed_offset();
    let build_id = build.id;

    // 却下は比較フェーズの完了時刻を動かさない（承認と同じ方針）。
    // 却下の時刻は比較ごとの `reviewed_at` に残る。
    let backfill = build.completed_at.is_none();
    let mut active: builds::ActiveModel = build.into();
    active.status = Set(BuildStatus::Rejected);
    if backfill {
        active.completed_at = Set(Some(now));
    }
    let updated = active.update(db).await?;

    // 未レビューの比較は rejected に倒す。
    comparisons::Entity::update_many()
        .col_expr(
            comparisons::Column::ReviewStatus,
            sea_orm::sea_query::Expr::value(ReviewStatus::Rejected),
        )
        .col_expr(
            comparisons::Column::ReviewedBy,
            sea_orm::sea_query::Expr::value(reviewer_id),
        )
        .col_expr(
            comparisons::Column::ReviewedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            comparisons::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(comparisons::Column::BuildId.eq(build_id))
        .filter(comparisons::Column::ReviewStatus.eq(ReviewStatus::Pending))
        .exec(db)
        .await?;

    Ok(updated)
}

/// 保持数の設定に従って古い完了ビルドを掃除する（ベストエフォート）。
///
/// プロジェクトの `build_retention_limit` が NULL（無制限）なら何もしない。
/// エラーはログに残すだけで呼び出し側の処理は失敗させないため、ビルド完了処理や
/// 設定更新の後処理からそのまま呼べる。
pub async fn prune_project_builds_best_effort(
    db: &DatabaseConnection,
    storage: &Arc<dyn StorageBackend>,
    project_id: Uuid,
) {
    let project = match projects::Entity::find_by_id(project_id).one(db).await {
        Ok(Some(project)) => project,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(%project_id, error = %e, "build pruning: failed to load project");
            return;
        }
    };
    let Some(limit) = project.build_retention_limit else {
        return;
    };
    match prune_old_builds(db, storage, project_id, limit).await {
        Ok(0) => {}
        Ok(deleted) => tracing::info!(%project_id, deleted, "pruned old builds"),
        Err(e) => tracing::warn!(%project_id, error = %e, "build pruning failed"),
    }
}

/// 完了（terminal 状態）ビルドを新しい順に `limit` 件残し、超過した古いものを削除する。
///
/// 削除対象からの除外:
///
/// - 現行 baseline の参照元ビルド（`baselines.source_build_id`）。baseline エントリは
///   ビルドのスクリーンショットと**同じストレージキーを共有**するため、参照元を消すと
///   baseline の実体まで失われる
/// - 進行中（非 terminal）のビルド。数えも消しもしない
///
/// 削除順序は「先に DB 行 → その後ストレージ」。DB は builds を消せば screenshots /
/// comparisons / build_logs が FK cascade で消える。ストレージ削除はベストエフォートで、
/// 失敗しても警告ログを残して続行する（既存の削除方針に合わせる）。
///
/// 戻り値は削除したビルド数。
pub async fn prune_old_builds(
    db: &DatabaseConnection,
    storage: &Arc<dyn StorageBackend>,
    project_id: Uuid,
    limit: i32,
) -> Result<u64, AppError> {
    if limit < 1 {
        return Ok(0);
    }

    // terminal 状態のビルドを新しい順に取得する。changes_detected は含めない
    // （レビュー待ちでパイプラインは終わっていないため、is_terminal と揃える）。
    let terminal = [
        BuildStatus::Passed,
        BuildStatus::Failed,
        BuildStatus::Approved,
        BuildStatus::Rejected,
    ];
    let builds = builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .filter(builds::Column::Status.is_in(terminal))
        .order_by_desc(builds::Column::Number)
        .all(db)
        .await?;

    if builds.len() <= limit as usize {
        return Ok(0);
    }

    // baseline に参照されているビルドは保護する。
    let protected: HashSet<Uuid> = baselines::Entity::find()
        .filter(baselines::Column::ProjectId.eq(project_id))
        .filter(baselines::Column::SourceBuildId.is_not_null())
        .all(db)
        .await?
        .into_iter()
        .filter_map(|baseline| baseline.source_build_id)
        .collect();

    let mut deleted = 0u64;
    for build in builds.into_iter().skip(limit as usize) {
        if protected.contains(&build.id) {
            continue;
        }

        // ストレージキーは DB 削除で cascade 消去される前に集めておく。
        let shots = screenshots::Entity::find()
            .filter(screenshots::Column::BuildId.eq(build.id))
            .all(db)
            .await?;
        let diff_keys: Vec<String> = comparisons::Entity::find()
            .filter(comparisons::Column::BuildId.eq(build.id))
            .all(db)
            .await?
            .into_iter()
            .filter_map(|comparison| comparison.diff_storage_key)
            .collect();
        let storybook_key = build.storybook_key.clone();

        // 先に DB 行を消す（screenshots / comparisons / build_logs は FK cascade）。
        builds::Entity::delete_by_id(build.id).exec(db).await?;

        // ストレージ削除はベストエフォート。失敗は警告ログのみで無視する。
        for shot in &shots {
            if let Err(e) = storage.delete(&shot.storage_key).await {
                tracing::warn!(
                    build_id = %build.id,
                    key = %shot.storage_key,
                    error = %e,
                    "failed to delete pruned screenshot object"
                );
            }
        }
        for key in &diff_keys {
            if let Err(e) = storage.delete(key).await {
                tracing::warn!(
                    build_id = %build.id,
                    key = %key,
                    error = %e,
                    "failed to delete pruned diff object"
                );
            }
        }
        if let Some(key) = &storybook_key
            && let Err(e) = storage.delete(key).await
        {
            tracing::warn!(
                build_id = %build.id,
                key = %key,
                error = %e,
                "failed to delete pruned storybook bundle"
            );
        }

        deleted += 1;
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_detect_differences() {
        assert!(!BuildCounts::default().has_differences());
        assert!(
            !BuildCounts {
                total: 3,
                unchanged: 3,
                ..Default::default()
            }
            .has_differences()
        );
        assert!(
            BuildCounts {
                total: 3,
                changed: 1,
                unchanged: 2,
                ..Default::default()
            }
            .has_differences()
        );
        assert!(
            BuildCounts {
                total: 1,
                added: 1,
                ..Default::default()
            }
            .has_differences()
        );
        assert!(
            BuildCounts {
                total: 1,
                removed: 1,
                ..Default::default()
            }
            .has_differences()
        );
    }
}
