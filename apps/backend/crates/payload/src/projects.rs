use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use validator::Validate;

use entity::projects;

/// `Option<Option<T>>` を「フィールド省略 = `None`」「`null` 送信 = `Some(None)`」
/// 「値送信 = `Some(Some(v))`」に分離してデシリアライズする。
///
/// 素の `Option<Option<T>>` は serde が最外の `null` を `None` に潰してしまい、
/// 「未指定（据え置き）」と「明示的な NULL 化」を区別できないため、`#[serde(default,
/// deserialize_with = "double_option")]` と併用してこの区別を復元する。
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

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
    /// storybook モードのレンダリングに使うビューポート幅（px）。
    pub viewport_width: i32,
    /// storybook モードのレンダリングに使うビューポート高さ（px）。
    pub viewport_height: i32,
    /// 保持する完了ビルド数の上限。null は無制限。
    #[schema(nullable)]
    pub build_retention_limit: Option<i32>,
    /// storybook モードの撮影時に `prefers-reduced-motion: reduce` を
    /// エミュレートするか。既定 false。有効化すると撮る絵が変わり、
    /// baseline が一度入れ替わる。
    pub emulate_reduced_motion: bool,
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
            viewport_width: model.viewport_width,
            viewport_height: model.viewport_height,
            build_retention_limit: model.build_retention_limit,
            emulate_reduced_motion: model.emulate_reduced_motion,
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
    /// storybook モードのレンダリングに使うビューポート幅（px、64〜10000）。
    #[validate(range(min = 64, max = 10000))]
    pub viewport_width: Option<i32>,
    /// storybook モードのレンダリングに使うビューポート高さ（px、64〜10000）。
    #[validate(range(min = 64, max = 10000))]
    pub viewport_height: Option<i32>,
    /// 保持する完了ビルド数の上限（1 以上）。`null` を送ると無制限に戻す。
    /// フィールド自体を省略すると現在値を据え置く。
    #[schema(nullable, value_type = Option<i32>)]
    #[serde(default, deserialize_with = "double_option")]
    pub build_retention_limit: Option<Option<i32>>,
    /// storybook モードの撮影時に `prefers-reduced-motion: reduce` を
    /// エミュレートするか。省略すると現在値を据え置く。既定は false。
    /// **有効化すると撮る絵が変わり、そのプロジェクトの baseline が
    /// 一度入れ替わる**——最初のビルドで差分をレビューして承認すること。
    pub emulate_reduced_motion: Option<bool>,
}
