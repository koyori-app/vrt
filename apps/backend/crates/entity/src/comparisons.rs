//! Comparisons entity — schema-first with hand-written `DeriveActiveEnum`。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 1 スクリーンショットの比較結果（`CompareBuildJob` が算出する）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize, ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(255))")]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "processing")]
    Processing,
    /// baseline と一致（しきい値以内）。
    #[sea_orm(string_value = "unchanged")]
    Unchanged,
    /// baseline と差分あり。
    #[sea_orm(string_value = "changed")]
    Changed,
    /// baseline に存在しない新規スクリーンショット。
    #[sea_orm(string_value = "added")]
    Added,
    /// baseline にはあるが今回のビルドに無い。
    #[sea_orm(string_value = "removed")]
    Removed,
    /// 比較自体が失敗した（画像が壊れている等）。
    #[sea_orm(string_value = "failed")]
    Failed,
}

impl ComparisonStatus {
    /// 人間のレビューを要する状態か（`unchanged` は自動承認）。
    pub fn needs_review(self) -> bool {
        matches!(
            self,
            ComparisonStatus::Changed
                | ComparisonStatus::Added
                | ComparisonStatus::Removed
                | ComparisonStatus::Failed
        )
    }
}

/// 人間のレビュー結果。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize, ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(255))")]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "approved")]
    Approved,
    #[sea_orm(string_value = "rejected")]
    Rejected,
}

pub use super::_generated::comparisons::*;

#[cfg(test)]
mod tests {
    use super::ComparisonStatus;

    #[test]
    fn unchanged_does_not_need_review() {
        assert!(!ComparisonStatus::Unchanged.needs_review());
        assert!(!ComparisonStatus::Pending.needs_review());
        assert!(ComparisonStatus::Changed.needs_review());
        assert!(ComparisonStatus::Added.needs_review());
        assert!(ComparisonStatus::Removed.needs_review());
    }
}
