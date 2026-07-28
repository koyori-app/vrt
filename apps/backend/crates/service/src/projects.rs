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

/// 0.0〜1.0 の比率パラメータを検証する。
pub fn validate_ratio(field: &str, value: f64) -> Result<(), AppError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(AppError::BadRequestDetail(format!(
            "{field} must be between 0.0 and 1.0"
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
    active.updated_at = Set(Utc::now().fixed_offset());
    Ok(active.update(db).await?)
}

/// プロジェクトを削除する。
pub async fn delete_project<C: ConnectionTrait>(db: &C, project_id: Uuid) -> Result<(), AppError> {
    projects::Entity::delete_by_id(project_id).exec(db).await?;
    Ok(())
}
