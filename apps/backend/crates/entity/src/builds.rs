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

/// 失敗が利用者の Story / テストに起因するか、VRT の実行環境に起因するか。
///
/// ビルドのライフサイクルとは直交する情報なので [`BuildStatus::Failed`] の
/// variant は増やさず、失敗したビルドだけがこの値を持つ。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize, ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(255))")]
#[serde(rename_all = "snake_case")]
pub enum BuildFailureOrigin {
    /// Story、play test、Storybook バンドルなど、利用者が直す対象。
    #[sea_orm(string_value = "test")]
    Test,
    /// Chromium、CDP、DB、Storage、worker など、VRT が直す対象。
    #[sea_orm(string_value = "vrt")]
    Vrt,
}

/// ビルドのライフサイクル。遷移は `service::builds::transition` に集約する。
///
/// ```text
/// screenshots モード:
/// pending ──finalize──▶ queued ──worker──▶ processing ──▶ passed | changes_detected | failed
///
/// storybook モード:
/// pending ──finalize──▶ queued ──worker──▶ rendering ──▶ processing ──▶ passed | changes_detected | failed
///                                              └──────────────────────▶ failed
///
/// どちらも: changes_detected ──▶ approved | rejected / passed ──▶ approved
///
/// 再実行（failed のみ）:
/// failed ──retry──▶ queued ──worker──▶ rendering（storybook） | processing（screenshots）
///
/// 再比較（比較が済んだビルドを現行 baseline と突き合わせ直す）:
/// changes_detected | passed ──recompare──▶ queued ──worker──▶ processing
/// ```
///
/// 再比較は撮影をやり直さない（screenshots は残っているので比較だけ差し替える）。
/// 入口は [`service::builds::recompare`] だけで、部分撮影ビルドは弾かれる——
/// 旧 baseline から複製した流用スクリーンショットが混ざったまま新しい baseline と
/// 比較すると、撮っていない story に偽の差分が出る。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize, ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(255))")]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// CI が作成した直後。スクリーンショット（または Storybook バンドル）のアップロードを受け付ける。
    #[sea_orm(string_value = "pending")]
    Pending,
    /// パイプラインの先頭ジョブをキューへ投入済みで、worker の取得待ち。
    /// storybook モードは render、screenshots モードは compare の待機を表す。
    #[sea_orm(string_value = "queued")]
    Queued,
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
    /// 終端状態（これ以上先へ進まない）か。
    ///
    /// `changes_detected` は**含まない**。パイプラインとしては終わっているが、
    /// レビューで `approved` / `rejected` に動くため。
    /// `failed` は含む——終端だが、唯一の例外として**再実行**
    /// （`service::builds::retry_failed`）でパイプラインの先頭へ戻れる。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            BuildStatus::Passed
                | BuildStatus::Failed
                | BuildStatus::Approved
                | BuildStatus::Rejected
        )
    }

    /// **パイプラインが完走した**状態か（= `builds.completed_at` を打つ状態）。
    ///
    /// ## `completed_at` のセマンティクス
    ///
    /// `completed_at` は「**自動処理が終わった時刻**」であって「レビューが終わった時刻」ではない。
    /// レンダリングと比較が終わって結果が確定した瞬間（`passed` / `changes_detected` / `failed`）に
    /// 一度だけ打ち、以降は上書きしない。例外は失敗ビルドの再実行
    /// （`service::builds::retry_failed`）で、パイプラインをやり直すので
    /// `completed_at` をクリアし、再完了時に打ち直す。
    ///
    /// こう決めた理由:
    ///
    /// - `created_at → completed_at` がそのまま「ビルドにかかった時間」になる。
    ///   人間が 3 日後に承認した時刻で上書きされると、この差分が意味を失う
    /// - レビュー結果には専用の列がある（`approved_by` / `approved_at`、
    ///   比較ごとの `reviewed_by` / `reviewed_at`）。`completed_at` に兼務させない
    ///
    /// 旧実装は [`is_terminal`](Self::is_terminal) で判定していたため、
    /// `changes_detected` だけ `completed_at` が NULL のまま残り、
    /// 逆に承認・却下の時刻で上書きされていた。
    pub fn completes_pipeline(self) -> bool {
        matches!(
            self,
            BuildStatus::Passed | BuildStatus::ChangesDetected | BuildStatus::Failed
        )
    }

    /// `self` から `to` への遷移が許可されているか。
    pub fn can_transition_to(self, to: BuildStatus) -> bool {
        use BuildStatus::*;
        match (self, to) {
            // finalize はまずキュー待ちへ入り、worker がモードに応じた処理状態へ進める。
            (Pending, Queued) => true,
            (Queued, Rendering | Processing) => true,
            (Rendering, Processing | Failed) => true,
            (Processing, Passed | ChangesDetected | Failed) => true,
            // レビュー結果の反映。差分検出済みのビルドだけがレビュー対象。
            (ChangesDetected, Approved | Rejected) => true,
            // 差分ゼロで通ったビルドも、baseline 昇格のために明示承認できる。
            (Passed, Approved) => true,
            // 失敗したビルドの再実行。どちらへ戻るかはモードで決まる
            // （`service::builds::retry_failed` が振り分ける）。
            (Failed, Queued) => true,
            // 比較が済んだビルドの再比較（`service::builds::recompare`）。
            // baseline が動いて承認できなくなったビルドを、撮り直さずに
            // 現行 baseline と突き合わせ直すための入口。approved / rejected
            // からは戻れない——レビューが確定した結果は書き換えない。
            (ChangesDetected | Passed, Queued) => true,
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
        assert!(Pending.can_transition_to(Queued));
        assert!(Queued.can_transition_to(Rendering));
        assert!(Rendering.can_transition_to(Processing));
        assert!(Rendering.can_transition_to(Failed));
        // レンダリング中のビルドは比較を飛ばして終端に行けない。
        assert!(!Rendering.can_transition_to(Passed));
        assert!(!Rendering.can_transition_to(ChangesDetected));
        assert!(!Rendering.can_transition_to(Approved));
        assert!(!Rendering.is_terminal());
        // 進行中から rendering に戻る経路は無い（failed からの再実行だけが戻れる）。
        assert!(!Processing.can_transition_to(Rendering));
    }

    #[test]
    fn retry_transitions() {
        // 再実行の入口は failed だけ。モードに応じてパイプラインの先頭へ戻る。
        assert!(Failed.can_transition_to(Queued));
        // 終端の結果を書き換える向きには戻れない。
        assert!(!Failed.can_transition_to(Passed));
        assert!(!Failed.can_transition_to(ChangesDetected));
        assert!(!Failed.can_transition_to(Pending));
        // レビューが確定した終端からは戻れない。
        assert!(!Approved.can_transition_to(Queued));
        assert!(!Rejected.can_transition_to(Queued));
    }

    #[test]
    fn recompare_transitions() {
        // 比較が済んだビルドは、撮り直さずに比較だけやり直せる。
        assert!(ChangesDetected.can_transition_to(Queued));
        assert!(Passed.can_transition_to(Queued));
        // 再比較で戻れるのは queued だけ。結果状態を直接書き換える向きは無い。
        assert!(!ChangesDetected.can_transition_to(Processing));
        assert!(!ChangesDetected.can_transition_to(Passed));
        assert!(!Passed.can_transition_to(ChangesDetected));
        assert!(!Passed.can_transition_to(Processing));
        // 処理中のビルドは queued へ巻き戻せない（二重投入の入口を作らない）。
        // pending → queued は finalize の正規経路なのでここでは扱わない。
        assert!(!Processing.can_transition_to(Queued));
        assert!(!Rendering.can_transition_to(Queued));
    }

    #[test]
    fn happy_path_transitions_are_allowed() {
        assert!(Pending.can_transition_to(Queued));
        assert!(Queued.can_transition_to(Processing));
        assert!(Processing.can_transition_to(Passed));
        assert!(Processing.can_transition_to(ChangesDetected));
        assert!(Processing.can_transition_to(Failed));
        assert!(ChangesDetected.can_transition_to(Approved));
        assert!(ChangesDetected.can_transition_to(Rejected));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(!Pending.can_transition_to(Passed));
        assert!(!Pending.can_transition_to(Rendering));
        assert!(!Pending.can_transition_to(Processing));
        assert!(!Pending.can_transition_to(Approved));
        assert!(!Processing.can_transition_to(Approved));
        assert!(!Approved.can_transition_to(Rejected));
        assert!(!Rejected.can_transition_to(Approved));
    }

    #[test]
    fn terminal_states() {
        assert!(Passed.is_terminal());
        assert!(Approved.is_terminal());
        assert!(!Pending.is_terminal());
        assert!(!Queued.is_terminal());
        assert!(!ChangesDetected.is_terminal());
    }
}
