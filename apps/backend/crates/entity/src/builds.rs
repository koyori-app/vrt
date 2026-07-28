//! Builds entity — schema-first with hand-written `DeriveActiveEnum`。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// ビルドの入力形式。
///
/// - [`BuildMode::Screenshots`]: CI が自前で撮った PNG をアップロードする（従来モード）
/// - [`BuildMode::Storybook`]: CI がビルド済み Storybook の zip をアップロードし、
///   サーバー側（`RenderBuildJob`）がヘッドレス Chromium で全ストーリーを撮る
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(255))")]
#[serde(rename_all = "snake_case")]
pub enum BuildMode {
    /// CI がスクリーンショットをアップロードする（既定）。
    #[default]
    #[sea_orm(string_value = "screenshots")]
    Screenshots,
    /// CI が storybook-static の zip をアップロードし、サーバーがレンダリングする。
    #[sea_orm(string_value = "storybook")]
    Storybook,
}

/// ビルドのライフサイクル。遷移は `service::builds::transition` に集約する。
///
/// ```text
/// screenshots モード:
/// pending ──finalize──▶ processing ──▶ passed | changes_detected | failed
///
/// storybook モード:
/// pending ──finalize──▶ rendering ──▶ processing ──▶ passed | changes_detected | failed
///                          └──────────────────────▶ failed
///
/// どちらも: changes_detected ──▶ approved | rejected / passed ──▶ approved
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize, ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(255))")]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// CI が作成した直後。スクリーンショット（または Storybook バンドル）のアップロードを受け付ける。
    #[sea_orm(string_value = "pending")]
    Pending,
    /// storybook モードで finalize 済み。`RenderBuildJob` がストーリーを撮影中。
    #[sea_orm(string_value = "rendering")]
    Rendering,
    /// finalize 済み。`CompareBuildJob` が処理中。
    #[sea_orm(string_value = "processing")]
    Processing,
    /// 差分なし。
    #[sea_orm(string_value = "passed")]
    Passed,
    /// 差分あり（レビュー待ち）。
    #[sea_orm(string_value = "changes_detected")]
    ChangesDetected,
    /// ジョブが回復不能なエラーで終了した。
    #[sea_orm(string_value = "failed")]
    Failed,
    /// レビューで承認され、baseline に昇格済み。
    #[sea_orm(string_value = "approved")]
    Approved,
    /// レビューで却下された。
    #[sea_orm(string_value = "rejected")]
    Rejected,
}

impl BuildStatus {
    /// 終端状態（これ以上遷移しない）か。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            BuildStatus::Passed
                | BuildStatus::Failed
                | BuildStatus::Approved
                | BuildStatus::Rejected
        )
    }

    /// `self` から `to` への遷移が許可されているか。
    pub fn can_transition_to(self, to: BuildStatus) -> bool {
        use BuildStatus::*;
        match (self, to) {
            (Pending, Processing) => true,
            // storybook モードの finalize。レンダリングが終わると processing に繋ぐ。
            (Pending, Rendering) => true,
            (Rendering, Processing | Failed) => true,
            (Processing, Passed | ChangesDetected | Failed) => true,
            // レビュー結果の反映。差分検出済みのビルドだけがレビュー対象。
            (ChangesDetected, Approved | Rejected) => true,
            // 差分ゼロで通ったビルドも、baseline 昇格のために明示承認できる。
            (Passed, Approved) => true,
            _ => false,
        }
    }
}

pub use super::_generated::builds::*;

#[cfg(test)]
mod tests {
    use super::BuildStatus::*;

    #[test]
    fn storybook_mode_transitions() {
        assert!(Pending.can_transition_to(Rendering));
        assert!(Rendering.can_transition_to(Processing));
        assert!(Rendering.can_transition_to(Failed));
        // レンダリング中のビルドは比較を飛ばして終端に行けない。
        assert!(!Rendering.can_transition_to(Passed));
        assert!(!Rendering.can_transition_to(ChangesDetected));
        assert!(!Rendering.can_transition_to(Approved));
        assert!(!Rendering.is_terminal());
        // rendering に戻る経路は無い。
        assert!(!Processing.can_transition_to(Rendering));
        assert!(!Failed.can_transition_to(Rendering));
    }

    #[test]
    fn happy_path_transitions_are_allowed() {
        assert!(Pending.can_transition_to(Processing));
        assert!(Processing.can_transition_to(Passed));
        assert!(Processing.can_transition_to(ChangesDetected));
        assert!(Processing.can_transition_to(Failed));
        assert!(ChangesDetected.can_transition_to(Approved));
        assert!(ChangesDetected.can_transition_to(Rejected));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(!Pending.can_transition_to(Passed));
        assert!(!Pending.can_transition_to(Approved));
        assert!(!Processing.can_transition_to(Approved));
        assert!(!Approved.can_transition_to(Rejected));
        assert!(!Failed.can_transition_to(Processing));
        assert!(!Rejected.can_transition_to(Approved));
    }

    #[test]
    fn terminal_states() {
        assert!(Passed.is_terminal());
        assert!(Approved.is_terminal());
        assert!(!Pending.is_terminal());
        assert!(!ChangesDetected.is_terminal());
    }
}
