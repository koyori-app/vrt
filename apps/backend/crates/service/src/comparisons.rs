//! 比較結果（1 スクリーンショット = 1 行）のレビュー操作。

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, prelude::Uuid,
};

use common::db::with_transaction;
use common::error::AppError;
use entity::{
    builds::BuildStatus, comparisons, comparisons::ComparisonStatus, comparisons::ReviewStatus,
};

/// レビュー操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    Approve,
    Reject,
}

impl ReviewAction {
    pub fn to_status(self) -> ReviewStatus {
        match self {
            ReviewAction::Approve => ReviewStatus::Approved,
            ReviewAction::Reject => ReviewStatus::Rejected,
        }
    }
}

/// ビルドの比較一覧（名前順）。
pub async fn list_for_build<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
) -> Result<Vec<comparisons::Model>, AppError> {
    Ok(comparisons::Entity::find()
        .filter(comparisons::Column::BuildId.eq(build_id))
        .order_by_asc(comparisons::Column::Name)
        .all(db)
        .await?)
}

/// 比較を ID で取得する。
pub async fn get_comparison<C: ConnectionTrait>(
    db: &C,
    comparison_id: Uuid,
) -> Result<comparisons::Model, AppError> {
    comparisons::Entity::find_by_id(comparison_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// 比較をレビューする。
///
/// - ビルドが `changes_detected` のときだけ受け付ける（それ以外は [`AppError::Conflict`]）
/// - `unchanged` の比較はそもそもレビュー不要なので [`AppError::Conflict`]
pub async fn review(
    db: &sea_orm::DatabaseConnection,
    build_id: Uuid,
    comparison_id: Uuid,
    action: ReviewAction,
    reviewer_id: Uuid,
) -> Result<comparisons::Model, AppError> {
    with_transaction(db, move |txn| {
        Box::pin(async move {
            let build = crate::review_lock::build(txn, build_id).await?;
            let comparison = crate::review_lock::comparison(txn, comparison_id).await?;

            if comparison.build_id != build.id {
                return Err(AppError::NotFound);
            }
            if build.status != BuildStatus::ChangesDetected {
                return Err(AppError::Conflict);
            }
            if !comparison.status.needs_review() {
                return Err(AppError::Conflict);
            }

            let now = Utc::now().fixed_offset();
            let mut active: comparisons::ActiveModel = comparison.into();
            active.review_status = Set(action.to_status());
            active.reviewed_by = Set(Some(reviewer_id));
            active.reviewed_at = Set(Some(now));
            active.updated_at = Set(now);
            Ok(active.update(txn).await?)
        })
    })
    .await
}

/// ビルドに紐づく比較を全削除する（ジョブのリトライ時に呼ぶ）。
pub async fn delete_for_build<C: ConnectionTrait>(db: &C, build_id: Uuid) -> Result<(), AppError> {
    comparisons::Entity::delete_many()
        .filter(comparisons::Column::BuildId.eq(build_id))
        .exec(db)
        .await?;
    Ok(())
}

/// `unchanged` は人手のレビュー不要なので自動承認しておく。
pub fn initial_review_status(status: ComparisonStatus) -> ReviewStatus {
    if status.needs_review() {
        ReviewStatus::Pending
    } else {
        ReviewStatus::Approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_comparisons_are_auto_approved() {
        assert_eq!(
            initial_review_status(ComparisonStatus::Unchanged),
            ReviewStatus::Approved
        );
        assert_eq!(
            initial_review_status(ComparisonStatus::Changed),
            ReviewStatus::Pending
        );
        assert_eq!(
            initial_review_status(ComparisonStatus::Added),
            ReviewStatus::Pending
        );
        assert_eq!(
            initial_review_status(ComparisonStatus::Removed),
            ReviewStatus::Pending
        );
    }

    #[test]
    fn review_action_maps_to_status() {
        assert_eq!(ReviewAction::Approve.to_status(), ReviewStatus::Approved);
        assert_eq!(ReviewAction::Reject.to_status(), ReviewStatus::Rejected);
    }
}
