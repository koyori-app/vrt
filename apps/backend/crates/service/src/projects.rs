//! プロジェクト（VRT の対象リポジトリ単位）の CRUD。すべてテナントにスコープされる。

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, prelude::Uuid,
};

use common::error::AppError;
use entity::projects;

use crate::tenants::validate_slug;

/// 既定のベースブランチ。
pub const DEFAULT_BRANCH: &str = "main";
/// 既定の 1 ピクセル差分しきい値（pixelmatch 互換）。
pub const DEFAULT_DIFF_THRESHOLD: f64 = 0.1;
/// 既定の失敗判定比率（0.0 = 1px でも差分があれば失敗扱い）。
pub const DEFAULT_DIFF_RATIO_FAIL: f64 = 0.0;
/// storybook モードのレンダリングに使う既定ビューポート幅。
pub const DEFAULT_VIEWPORT_WIDTH: i32 = 1280;
/// storybook モードのレンダリングに使う既定ビューポート高さ。
pub const DEFAULT_VIEWPORT_HEIGHT: i32 = 720;
/// ビューポートに指定できる下限（px）。
pub const MIN_VIEWPORT: i32 = 64;
/// ビューポートに指定できる上限（px）。`screenshots::MAX_DIMENSION` と揃える。
pub const MAX_VIEWPORT: i32 = 10_000;

/// 0.0〜1.0 の比率パラメータを検証する。
pub fn validate_ratio(field: &str, value: f64) -> Result<(), AppError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(AppError::BadRequestDetail(format!(
            "{field} must be between 0.0 and 1.0"
        )));
    }
    Ok(())
}

/// ビューポート寸法を検証する。
pub fn validate_viewport(field: &str, value: i32) -> Result<(), AppError> {
    if !(MIN_VIEWPORT..=MAX_VIEWPORT).contains(&value) {
        return Err(AppError::BadRequestDetail(format!(
            "{field} must be between {MIN_VIEWPORT} and {MAX_VIEWPORT}"
        )));
    }
    Ok(())
}

/// ビルド保持数の上限を検証する（1 以上）。
pub fn validate_retention_limit(field: &str, value: i32) -> Result<(), AppError> {
    if value < 1 {
        return Err(AppError::BadRequestDetail(format!(
            "{field} must be at least 1"
        )));
    }
    Ok(())
}

/// 更新可能なプロジェクト設定。`None` のフィールドは据え置き。
#[derive(Debug, Default, Clone)]
pub struct ProjectSettings {
    pub name: Option<String>,
    pub default_branch: Option<String>,
    pub diff_threshold: Option<f64>,
    pub diff_ratio_fail: Option<f64>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    /// ビルド保持数の上限。外側 `None` は据え置き、`Some(None)` は無制限（NULL）に設定、
    /// `Some(Some(n))` は上限を `n` に設定する。
    pub build_retention_limit: Option<Option<i32>>,
}

/// テナント内のプロジェクト一覧（作成順）。
pub async fn list_projects<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Vec<projects::Model>, AppError> {
    Ok(projects::Entity::find()
        .filter(projects::Column::TenantId.eq(tenant_id))
        .order_by_asc(projects::Column::CreatedAt)
        .all(db)
        .await?)
}

/// プロジェクトを ID で取得する。
pub async fn get_project<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
) -> Result<projects::Model, AppError> {
    projects::Entity::find_by_id(project_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// テナント slug + プロジェクト slug でプロジェクトを解決する（CI エンドポイント用）。
///
/// CI は UUID を知らないため、`{tenant_slug}/{project_slug}` でプロジェクトを指す。
pub async fn get_project_by_slug<C: ConnectionTrait>(
    db: &C,
    tenant_slug: &str,
    project_slug: &str,
) -> Result<projects::Model, AppError> {
    let tenant = entity::tenants::Entity::find()
        .filter(entity::tenants::Column::Slug.eq(tenant_slug))
        .one(db)
        .await?
        .ok_or(AppError::NotFound)?;

    projects::Entity::find()
        .filter(projects::Column::TenantId.eq(tenant.id))
        .filter(projects::Column::Slug.eq(project_slug))
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// テナントに属するプロジェクトを取得する（他テナントのものは 404）。
pub async fn get_project_in_tenant<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    project_id: Uuid,
) -> Result<projects::Model, AppError> {
    projects::Entity::find_by_id(project_id)
        .filter(projects::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// プロジェクトを作成する。テナント内で slug が重複すれば 409。
pub async fn create_project<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    name: String,
    slug: String,
    default_branch: Option<String>,
) -> Result<projects::Model, AppError> {
    validate_slug(&slug)?;

    let duplicate = projects::Entity::find()
        .filter(projects::Column::TenantId.eq(tenant_id))
        .filter(projects::Column::Slug.eq(slug.clone()))
        .one(db)
        .await?;
    if duplicate.is_some() {
        return Err(AppError::Conflict);
    }

    let now = Utc::now().fixed_offset();
    Ok(projects::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        name: Set(name),
        slug: Set(slug),
        default_branch: Set(default_branch.unwrap_or_else(|| DEFAULT_BRANCH.to_string())),
        diff_threshold: Set(DEFAULT_DIFF_THRESHOLD),
        diff_ratio_fail: Set(DEFAULT_DIFF_RATIO_FAIL),
        viewport_width: Set(DEFAULT_VIEWPORT_WIDTH),
        viewport_height: Set(DEFAULT_VIEWPORT_HEIGHT),
        build_retention_limit: Set(None),
        github_installation_id: Set(None),
        github_repo: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?)
}

/// プロジェクト設定を更新する。
pub async fn update_project<C: ConnectionTrait>(
    db: &C,
    project: projects::Model,
    settings: ProjectSettings,
) -> Result<projects::Model, AppError> {
    if let Some(value) = settings.diff_threshold {
        validate_ratio("diff_threshold", value)?;
    }
    if let Some(value) = settings.diff_ratio_fail {
        validate_ratio("diff_ratio_fail", value)?;
    }
    if let Some(value) = settings.viewport_width {
        validate_viewport("viewport_width", value)?;
    }
    if let Some(value) = settings.viewport_height {
        validate_viewport("viewport_height", value)?;
    }
    if let Some(Some(value)) = settings.build_retention_limit {
        validate_retention_limit("build_retention_limit", value)?;
    }

    let mut active: projects::ActiveModel = project.into();
    if let Some(name) = settings.name {
        active.name = Set(name);
    }
    if let Some(default_branch) = settings.default_branch {
        active.default_branch = Set(default_branch);
    }
    if let Some(diff_threshold) = settings.diff_threshold {
        active.diff_threshold = Set(diff_threshold);
    }
    if let Some(diff_ratio_fail) = settings.diff_ratio_fail {
        active.diff_ratio_fail = Set(diff_ratio_fail);
    }
    if let Some(viewport_width) = settings.viewport_width {
        active.viewport_width = Set(viewport_width);
    }
    if let Some(viewport_height) = settings.viewport_height {
        active.viewport_height = Set(viewport_height);
    }
    if let Some(build_retention_limit) = settings.build_retention_limit {
        active.build_retention_limit = Set(build_retention_limit);
    }
    active.updated_at = Set(Utc::now().fixed_offset());
    Ok(active.update(db).await?)
}

/// プロジェクトを削除する。
pub async fn delete_project<C: ConnectionTrait>(db: &C, project_id: Uuid) -> Result<(), AppError> {
    projects::Entity::delete_by_id(project_id).exec(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_bounds_are_enforced() {
        assert!(validate_viewport("viewport_width", DEFAULT_VIEWPORT_WIDTH).is_ok());
        assert!(validate_viewport("viewport_width", MIN_VIEWPORT).is_ok());
        assert!(validate_viewport("viewport_width", MAX_VIEWPORT).is_ok());
        assert!(validate_viewport("viewport_width", MIN_VIEWPORT - 1).is_err());
        assert!(validate_viewport("viewport_height", MAX_VIEWPORT + 1).is_err());
        assert!(validate_viewport("viewport_height", 0).is_err());
        assert!(validate_viewport("viewport_height", -1).is_err());
    }
}
