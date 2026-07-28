use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

use entity::projects;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub tenant_id: Uuid,
    pub name: String,
    /// テナント内で一意な URL 断片。
    pub slug: String,
    pub default_branch: String,
    /// 1 ピクセルを差分と判定する色距離のしきい値（0.0〜1.0）。
    pub diff_threshold: f64,
    /// ビルドを失敗扱いにする差分ピクセル比率（0.0〜1.0）。
    pub diff_ratio_fail: f64,
    #[schema(nullable)]
    pub github_installation_id: Option<i64>,
    #[schema(nullable)]
    pub github_repo: Option<String>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: DateTime<Utc>,
}

impl From<projects::Model> for ProjectResponse {
    fn from(model: projects::Model) -> Self {
        Self {
            id: model.id,
            tenant_id: model.tenant_id,
            name: model.name,
            slug: model.slug,
            default_branch: model.default_branch,
            diff_threshold: model.diff_threshold,
            diff_ratio_fail: model.diff_ratio_fail,
            github_installation_id: model.github_installation_id,
            github_repo: model.github_repo,
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct CreateProjectRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    /// 小文字英数とハイフンのみ。テナント内で一意。
    #[validate(length(min = 2, max = 63))]
    pub slug: String,
    #[validate(length(min = 1, max = 255))]
    pub default_branch: Option<String>,
}

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct UpdateProjectRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    #[validate(length(min = 1, max = 255))]
    pub default_branch: Option<String>,
    #[validate(range(min = 0.0, max = 1.0))]
    pub diff_threshold: Option<f64>,
    #[validate(range(min = 0.0, max = 1.0))]
    pub diff_ratio_fail: Option<f64>,
}
