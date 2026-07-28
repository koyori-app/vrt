//! baseline（承認済みスクリーンショット集合）の解決。

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, prelude::Uuid};

use common::error::AppError;
use entity::{baseline_entries, baselines, projects};

/// 比較に使う baseline を解決する。
///
/// 1. 同一ブランチの最新 baseline
/// 2. 無ければプロジェクトのデフォルトブランチの最新 baseline
/// 3. それも無ければ `None`（＝初回ビルド。全スクリーンショットが `added` になる）
pub async fn latest_for<C: ConnectionTrait>(
    db: &C,
    project: &projects::Model,
    branch: &str,
) -> Result<Option<baselines::Model>, AppError> {
    if let Some(found) = latest_on_branch(db, project.id, branch).await? {
        return Ok(Some(found));
    }
    if branch != project.default_branch {
        return latest_on_branch(db, project.id, &project.default_branch).await;
    }
    Ok(None)
}

/// 指定ブランチの最新 baseline。
pub async fn latest_on_branch<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
    branch: &str,
) -> Result<Option<baselines::Model>, AppError> {
    Ok(baselines::Entity::find()
        .filter(baselines::Column::ProjectId.eq(project_id))
        .filter(baselines::Column::Branch.eq(branch))
        .order_by_desc(baselines::Column::CreatedAt)
        .order_by_desc(baselines::Column::Id)
        .one(db)
        .await?)
}

/// baseline のエントリ一覧（名前順）。
pub async fn entries<C: ConnectionTrait>(
    db: &C,
    baseline_id: Uuid,
) -> Result<Vec<baseline_entries::Model>, AppError> {
    Ok(baseline_entries::Entity::find()
        .filter(baseline_entries::Column::BaselineId.eq(baseline_id))
        .order_by_asc(baseline_entries::Column::Name)
        .all(db)
        .await?)
}

/// baseline エントリを ID で取得する。
pub async fn get_entry<C: ConnectionTrait>(
    db: &C,
    entry_id: Uuid,
) -> Result<baseline_entries::Model, AppError> {
    baseline_entries::Entity::find_by_id(entry_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// baseline を ID で取得する。
pub async fn get_baseline<C: ConnectionTrait>(
    db: &C,
    baseline_id: Uuid,
) -> Result<baselines::Model, AppError> {
    baselines::Entity::find_by_id(baseline_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}
