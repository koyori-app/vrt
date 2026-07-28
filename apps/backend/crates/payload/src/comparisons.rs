//! 比較結果関連の DTO。

use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use entity::{comparisons, comparisons::ComparisonStatus, comparisons::ReviewStatus};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ComparisonResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub build_id: Uuid,
    pub name: String,
    /// 今回のスクリーンショット。`removed` のときは null。
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub screenshot_id: Option<Uuid>,
    /// 比較元の baseline エントリ。`added` のときは null。
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub baseline_entry_id: Option<Uuid>,
    pub status: ComparisonStatus,
    pub review_status: ReviewStatus,
    /// 差分画像があるか（実体は `/v1/comparisons/{id}/diff-content` で取得する）。
    pub has_diff_image: bool,
    #[schema(nullable)]
    pub diff_pixel_count: Option<i64>,
    #[schema(nullable)]
    pub diff_ratio: Option<f64>,
    #[schema(nullable)]
    pub error_message: Option<String>,
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub reviewed_by: Option<Uuid>,
    #[schema(value_type = Option<String>, format = "date-time", nullable)]
    pub reviewed_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: DateTime<Utc>,
}

impl From<comparisons::Model> for ComparisonResponse {
    fn from(model: comparisons::Model) -> Self {
        Self {
            id: model.id,
            build_id: model.build_id,
            name: model.name,
            screenshot_id: model.screenshot_id,
            baseline_entry_id: model.baseline_entry_id,
            status: model.status,
            review_status: model.review_status,
            has_diff_image: model.diff_storage_key.is_some(),
            diff_pixel_count: model.diff_pixel_count,
            diff_ratio: model.diff_ratio,
            error_message: model.error_message,
            reviewed_by: model.reviewed_by,
            reviewed_at: model.reviewed_at.map(|t| t.with_timezone(&Utc)),
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ComparisonListResponse {
    pub comparisons: Vec<ComparisonResponse>,
    pub total: u64,
}

/// レビュー操作。service 側の `ReviewAction` へは handler で変換する
/// （payload クレートは service に依存しない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReviewActionRequest {
    Approve,
    Reject,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReviewComparisonRequest {
    pub action: ReviewActionRequest,
}
