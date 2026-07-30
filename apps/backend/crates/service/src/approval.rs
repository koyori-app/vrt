//! ビルド承認の可否を決める純関数群。
//!
//! [`crate::builds::approve_build`] はここで決めた結果に従って baseline を作る。
//! 判定を DB アクセスから切り離してあるのは、承認ガードが**偽陰性**（本来止めるべき
//! 承認を通してしまう）を起こしていないことを DB 無しの単体テストで固定するため。

use std::collections::HashSet;

use sea_orm::prelude::Uuid;

use entity::comparisons::{ComparisonStatus, ReviewStatus};

/// 承認リクエストのオプション。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApproveOptions {
    /// 未レビューの比較をまとめて承認する（`removed` は含めない）。
    pub force: bool,
    /// `removed`（baseline から story が消える）を承認対象に含める。
    ///
    /// story の消滅は不可逆なので [`force`](Self::force) とは別の明示フラグにしてある。
    pub accept_removals: bool,
}

impl ApproveOptions {
    pub fn force() -> Self {
        Self {
            force: true,
            accept_removals: false,
        }
    }
}

/// 承認判定に必要な 1 比較ぶんの情報。
///
/// `comparisons::Model` から必要な列だけを写したもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonFacts {
    pub name: String,
    pub status: ComparisonStatus,
    pub review_status: ReviewStatus,
}

impl ComparisonFacts {
    pub fn new(name: impl Into<String>, status: ComparisonStatus, review: ReviewStatus) -> Self {
        Self {
            name: name.into(),
            status,
            review_status: review,
        }
    }
}

impl From<&entity::comparisons::Model> for ComparisonFacts {
    fn from(model: &entity::comparisons::Model) -> Self {
        Self {
            name: model.name.clone(),
            status: model.status,
            review_status: model.review_status,
        }
    }
}

/// 却下された比較の名前（名前順・重複なし）。
///
/// 1 件でもあればビルドは承認できない。ここを素通しすると
/// **却下したはずのスクリーンショットが baseline に焼き付き**、以降そのズレが「正」になる。
/// `unchanged` は自動承認なので却下されようがないが、判定は
/// [`ComparisonStatus::needs_review`] で絞って意図を明示しておく。
pub fn rejected_names(comparisons: &[ComparisonFacts]) -> Vec<String> {
    let mut names: Vec<String> = comparisons
        .iter()
        .filter(|c| c.status.needs_review() && c.review_status == ReviewStatus::Rejected)
        .map(|c| c.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// エラーメッセージ用に名前を最大 `max` 件だけ並べる（残りは `(+N more)`）。
pub fn summarize_names(names: &[String], max: usize) -> String {
    if names.len() <= max {
        return names.join(", ");
    }
    let shown = names[..max].join(", ");
    format!("{shown} (+{} more)", names.len() - max)
}

/// baseline に載っていたのに今回のビルドから消えた story 名（名前順）。
///
/// `approved_removals` は「レビューで消滅を承認した」名前。ここに無い欠落は
/// 撮影漏れ・アップロード失敗と区別がつかないため、承認を止める材料になる。
pub fn unexpected_missing_names(
    baseline_names: &[String],
    shot_names: &HashSet<String>,
    approved_removals: &HashSet<String>,
) -> Vec<String> {
    let mut names: Vec<String> = baseline_names
        .iter()
        .filter(|name| !shot_names.contains(*name) && !approved_removals.contains(*name))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
}

/// 承認済みの `removed` 比較の名前集合。
pub fn approved_removal_names(comparisons: &[ComparisonFacts]) -> HashSet<String> {
    comparisons
        .iter()
        .filter(|c| {
            c.status == ComparisonStatus::Removed && c.review_status == ReviewStatus::Approved
        })
        .map(|c| c.name.clone())
        .collect()
}

/// 未レビューのまま残り、承認を止めるべき比較の名前（名前順）。
///
/// - `force == false`: 人手判断が要る未レビューはすべてブロックする（従来どおり）
/// - `force == true`: 一括承認は `removed` を巻き込まない。`removed` の未レビューは
///   `accept_removals` を明示したときだけ通す
pub fn blocking_pending_names(
    comparisons: &[ComparisonFacts],
    options: ApproveOptions,
) -> Vec<String> {
    let mut names: Vec<String> = comparisons
        .iter()
        .filter(|c| c.status.needs_review() && c.review_status == ReviewStatus::Pending)
        .filter(|c| !is_bulk_approvable(c.status, options))
        .map(|c| c.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `force` の一括承認が触ってよい比較か。
///
/// `removed` は「story を baseline から消す」不可逆操作なので、まとめ承認には含めない。
/// `accept_removals` を明示したときだけ対象に加える。
pub fn is_bulk_approvable(status: ComparisonStatus, options: ApproveOptions) -> bool {
    if !options.force || !status.needs_review() {
        return false;
    }
    status != ComparisonStatus::Removed || options.accept_removals
}

/// 承認しようとしているビルドが、現行 baseline の生成元より古いか。
///
/// 古いビルドを後追いで承認すると、新しい baseline が古いスクリーンショットで
/// 上書きされて**巻き戻る**。`baseline_source_number` が `None`（生成元不明・
/// baseline 無し）のときは判定材料が無いので `false`。
pub fn is_older_than_baseline_source(
    build_number: i64,
    baseline_source_number: Option<i64>,
) -> bool {
    baseline_source_number.is_some_and(|source| build_number < source)
}

/// このビルドが比較に使った baseline が、いまも最新かどうか。
///
/// `build.baseline_id` は比較ジョブが `baselines::latest_for` で解決した結果。
/// これが現在の解決結果と食い違っていたら、比較後に別の承認が baseline を進めている。
/// そのまま承認すると「見ていない baseline」に対する差分を焼き付けることになる。
pub fn baseline_is_current(
    build_baseline_id: Option<Uuid>,
    current_baseline_id: Option<Uuid>,
) -> bool {
    build_baseline_id == current_baseline_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(items: &[(&str, ComparisonStatus, ReviewStatus)]) -> Vec<ComparisonFacts> {
        items
            .iter()
            .map(|(n, s, r)| ComparisonFacts::new(*n, *s, *r))
            .collect()
    }

    // ---- 穴①: 却下された比較が baseline へ昇格する ----

    #[test]
    fn rejected_comparison_blocks_approval() {
        let list = facts(&[
            ("home", ComparisonStatus::Changed, ReviewStatus::Approved),
            ("login", ComparisonStatus::Changed, ReviewStatus::Rejected),
            ("about", ComparisonStatus::Unchanged, ReviewStatus::Approved),
        ]);
        assert_eq!(rejected_names(&list), vec!["login".to_string()]);
    }

    #[test]
    fn no_rejection_means_no_block() {
        let list = facts(&[
            ("home", ComparisonStatus::Changed, ReviewStatus::Approved),
            ("about", ComparisonStatus::Unchanged, ReviewStatus::Approved),
            ("new", ComparisonStatus::Added, ReviewStatus::Approved),
        ]);
        assert!(rejected_names(&list).is_empty());
    }

    #[test]
    fn rejected_names_are_sorted_and_deduped() {
        let list = facts(&[
            ("z", ComparisonStatus::Changed, ReviewStatus::Rejected),
            ("a", ComparisonStatus::Added, ReviewStatus::Rejected),
            ("a", ComparisonStatus::Removed, ReviewStatus::Rejected),
        ]);
        assert_eq!(
            rejected_names(&list),
            vec!["a".to_string(), "z".to_string()]
        );
    }

    #[test]
    fn summarize_truncates_long_lists() {
        let names: Vec<String> = (0..5).map(|i| format!("s{i}")).collect();
        assert_eq!(summarize_names(&names, 10), "s0, s1, s2, s3, s4");
        assert_eq!(summarize_names(&names, 2), "s0, s1 (+3 more)");
    }

    // ---- 穴②: 古いビルドの後追い承認で baseline が巻き戻る ----

    #[test]
    fn older_build_than_baseline_source_is_rejected() {
        // baseline は #7 のビルドから作られている。#3 を後から承認すると巻き戻る。
        assert!(is_older_than_baseline_source(3, Some(7)));
        // 同じビルドの再承認・より新しいビルドは巻き戻らない。
        assert!(!is_older_than_baseline_source(7, Some(7)));
        assert!(!is_older_than_baseline_source(9, Some(7)));
        // 生成元が分からない（baseline 無し / 旧データ）なら止めない。
        assert!(!is_older_than_baseline_source(3, None));
    }

    #[test]
    fn baseline_moved_since_comparison_is_detected() {
        let d = Uuid::new_v4();
        let e = Uuid::new_v4();
        // 比較に使った baseline がいまも最新。
        assert!(baseline_is_current(Some(d), Some(d)));
        // 比較後に別の承認が baseline を進めた。
        assert!(!baseline_is_current(Some(d), Some(e)));
        // 初回ビルド（baseline 無し）同士。
        assert!(baseline_is_current(None, None));
        // 比較時は baseline が無かったが、その後に作られた。
        assert!(!baseline_is_current(None, Some(e)));
    }

    // ---- 穴③: force が removed まで一括承認する ----

    #[test]
    fn force_does_not_bulk_approve_removals() {
        let opts = ApproveOptions::force();
        assert!(is_bulk_approvable(ComparisonStatus::Changed, opts));
        assert!(is_bulk_approvable(ComparisonStatus::Added, opts));
        assert!(is_bulk_approvable(ComparisonStatus::Failed, opts));
        assert!(
            !is_bulk_approvable(ComparisonStatus::Removed, opts),
            "removed は force だけでは承認できない"
        );
    }

    #[test]
    fn accept_removals_opts_into_removal_approval() {
        let opts = ApproveOptions {
            force: true,
            accept_removals: true,
        };
        assert!(is_bulk_approvable(ComparisonStatus::Removed, opts));
    }

    #[test]
    fn force_without_accept_removals_still_blocks_on_removed() {
        let list = facts(&[
            ("home", ComparisonStatus::Changed, ReviewStatus::Pending),
            ("legacy", ComparisonStatus::Removed, ReviewStatus::Pending),
        ]);
        assert_eq!(
            blocking_pending_names(&list, ApproveOptions::force()),
            vec!["legacy".to_string()],
            "changed は force で流せるが removed は残る"
        );
        assert!(
            blocking_pending_names(
                &list,
                ApproveOptions {
                    force: true,
                    accept_removals: true,
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn without_force_every_pending_blocks() {
        let list = facts(&[
            ("home", ComparisonStatus::Changed, ReviewStatus::Pending),
            ("legacy", ComparisonStatus::Removed, ReviewStatus::Pending),
            ("about", ComparisonStatus::Unchanged, ReviewStatus::Approved),
        ]);
        assert_eq!(
            blocking_pending_names(&list, ApproveOptions::default()),
            vec!["home".to_string(), "legacy".to_string()]
        );
    }

    #[test]
    fn reviewed_comparisons_do_not_block() {
        let list = facts(&[
            ("home", ComparisonStatus::Changed, ReviewStatus::Approved),
            ("legacy", ComparisonStatus::Removed, ReviewStatus::Approved),
        ]);
        assert!(blocking_pending_names(&list, ApproveOptions::default()).is_empty());
    }

    // ---- 穴③: baseline manifest と今回のビルドの照合 ----

    #[test]
    fn silent_disappearance_is_reported() {
        let baseline = vec!["about".to_string(), "home".to_string(), "login".to_string()];
        let shots: HashSet<String> = ["home".to_string()].into_iter().collect();
        // 「消えてよい」と承認されたのは login だけ。about は説明のつかない欠落。
        let approved: HashSet<String> = ["login".to_string()].into_iter().collect();
        assert_eq!(
            unexpected_missing_names(&baseline, &shots, &approved),
            vec!["about".to_string()]
        );
    }

    #[test]
    fn fully_covered_removals_pass() {
        let baseline = vec!["home".to_string(), "legacy".to_string()];
        let shots: HashSet<String> = ["home".to_string()].into_iter().collect();
        let approved: HashSet<String> = ["legacy".to_string()].into_iter().collect();
        assert!(unexpected_missing_names(&baseline, &shots, &approved).is_empty());
    }

    #[test]
    fn approved_removal_names_only_collects_approved_removals() {
        let list = facts(&[
            ("gone", ComparisonStatus::Removed, ReviewStatus::Approved),
            ("stay", ComparisonStatus::Removed, ReviewStatus::Pending),
            ("changed", ComparisonStatus::Changed, ReviewStatus::Approved),
        ]);
        let names = approved_removal_names(&list);
        assert_eq!(names.len(), 1);
        assert!(names.contains("gone"));
    }
}
