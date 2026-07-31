//! 「撮らない」選択計画の組み立てと JSON 契約。
//!
//! `screenshots` モードでは撮影そのものが CI 側のテストランナーの仕事であり、
//! サーバーはレンダリングしない。そのため CLI は撮影を代行せず、
//! 「どの story を撮ればよいか」だけを機械可読な JSON で出力し、
//! 呼び出し側（CI）がその集合を自身のテスト選択形式へ翻訳して撮る。
//!
//! 影響 story の算出そのものは [`crate::turbosnap`] を再利用する。ここに置くのは
//!
//! - stats / index の読み込みと、読めなかったときの倒し方（fail-closed）
//! - CI へ渡す JSON の形（[`PlanDocument`]）
//!
//! の二つだけで、グラフ探索のロジックは二度書かない。
//!
//! `storybook` モードの `vrt upload` も同じ読み込み経路を通す。両モードで
//! 選択結果が食い違わないようにするためで、両者の違いは
//! 「入力が壊れていたときにエラーへ倒すか全撮影へ倒すか」（[`CorruptInput`]）だけである。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::turbosnap::{self, Plan, StoryEntry, WebpackStats};

/// 選択計画 JSON の契約 version。
///
/// CI 側は未知の version を見たら計画を捨てて全撮影へ倒す。互換を壊す変更を
/// 入れるときだけ上げる。
pub const PLAN_VERSION: u32 = 1;

/// 計画の種別。
///
/// `Only` の `story_ids` が空であることと `CaptureAll` は別物である。前者は
/// 「影響のある既存 story は無い」という選択結果、後者は「選択を諦めた」である。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    /// 列挙した story だけ撮る。
    Only,
    /// 全 story を撮る。
    CaptureAll,
}

/// 選択結果。`notes` は判断の補足（無視したファイル等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// 全撮影へ倒した。`reason` はその理由。
    CaptureAll { reason: String, notes: Vec<String> },
    /// 列挙した story だけ撮る。空 Vec は「撮るべき既存 story 無し」。
    Only {
        story_ids: Vec<String>,
        notes: Vec<String>,
    },
}

/// stats / index が読めない・壊れているときの倒し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorruptInput {
    /// 全撮影へ倒す。撮り逃しを作らない側に寄せる（`screenshots` の選択計画）。
    FailClosed,
    /// エラーとして返す（`storybook` の `vrt upload` の既存挙動）。
    Error,
}

/// 選択に要る入力の在り処と、突き合わせ用の座標。
///
/// `stats_json` / `index_json` を省略したときは `dir` 配下の既定名
/// （`preview-stats.json` / `index.json`）を見る。
pub struct SelectionInputs<'a> {
    pub dir: &'a Path,
    pub stats_json: Option<&'a Path>,
    pub index_json: Option<&'a Path>,
    /// `git rev-parse --show-toplevel`。
    pub repo_root: &'a Path,
    /// storybook build を回した cwd。stats のモジュール名はここ相対。
    pub cwd: &'a Path,
    /// baseline から HEAD までの変更ファイル（リポジトリルート相対）。
    pub changed_files: &'a [String],
}

impl SelectionInputs<'_> {
    fn stats_path(&self) -> PathBuf {
        self.stats_json
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.dir.join("preview-stats.json"))
    }

    fn index_path(&self) -> PathBuf {
        self.index_json
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.dir.join("index.json"))
    }
}

/// stats と index を読み、[`turbosnap::compute_affected_stories`] で選択する。
///
/// stats が存在しない場合は `on_corrupt` に関わらず全撮影へ倒す（差分撮影を
/// 有効にする手順を理由に載せる）。読めたが壊れている場合の扱いだけを
/// `on_corrupt` で切り替える。
pub fn select_stories(inputs: &SelectionInputs<'_>, on_corrupt: CorruptInput) -> Result<Selection> {
    let mut notes: Vec<String> = Vec::new();

    let stats_path = inputs.stats_path();
    if !stats_path.is_file() {
        return Ok(Selection::CaptureAll {
            reason: format!(
                "stats file {} not found. \
                 Run `storybook build --stats-json` to enable per-story capture",
                stats_path.display()
            ),
            notes,
        });
    }
    let stats = match load_stats(&stats_path) {
        Ok(stats) => stats,
        Err(e) => return on_corrupt_input(on_corrupt, e, notes),
    };

    let index_path = inputs.index_path();
    let stories = match load_index(&index_path) {
        Ok(stories) => stories,
        Err(e) => return on_corrupt_input(on_corrupt, e, notes),
    };

    let outcome = turbosnap::compute_affected_stories(
        inputs.repo_root,
        inputs.cwd,
        inputs.changed_files,
        &stats,
        &stories,
    );
    notes.extend(outcome.notes);
    Ok(match outcome.plan {
        Plan::CaptureAll(reason) => Selection::CaptureAll { reason, notes },
        Plan::Only(story_ids) => Selection::Only { story_ids, notes },
    })
}

/// 壊れた入力を、方針に従ってエラーか全撮影へ倒す。
fn on_corrupt_input(
    policy: CorruptInput,
    error: anyhow::Error,
    notes: Vec<String>,
) -> Result<Selection> {
    match policy {
        CorruptInput::Error => Err(error),
        CorruptInput::FailClosed => Ok(Selection::CaptureAll {
            reason: format!("{error:#}"),
            notes,
        }),
    }
}

fn load_stats(path: &Path) -> Result<WebpackStats> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    WebpackStats::parse(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn load_index(path: &Path) -> Result<Vec<StoryEntry>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    turbosnap::parse_index(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

/// 計画を固定するビルド座標。
///
/// baseline と HEAD の両方を計画へ焼き付ける。CI は撮影前にこの二つが
/// 変わっていないか確かめ、変わっていたら計画を捨てて全撮影へ倒す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCoordinates {
    pub branch: String,
    pub baseline_commit_sha: Option<String>,
    pub head_commit_sha: String,
    /// 計画のために作成した screenshots ビルド ID。
    /// baseline を明示指定した（ビルドを作らなかった）場合は `None`。
    pub build_id: Option<String>,
}

/// CI へ渡す選択計画。
///
/// `plan = "capture_all"` のときは `story_ids` を載せず `reason` を残す。
/// `plan = "only"` のときは `story_ids` を必ず載せる（空配列を含む）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDocument {
    pub version: u32,
    pub plan: PlanKind,
    pub branch: String,
    pub baseline_commit_sha: Option<String>,
    pub head_commit_sha: String,
    /// 撮る story ID。`plan = "only"` のときだけ載る。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_ids: Option<Vec<String>>,
    /// 全撮影へ倒した理由。`plan = "only"` では `null`。
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
}

impl PlanDocument {
    /// 全撮影の計画。選択を試みる前に諦めた場合（baseline 無し・git 差分不能）に使う。
    pub fn capture_all(coords: PlanCoordinates, reason: String, notes: Vec<String>) -> Self {
        Self {
            version: PLAN_VERSION,
            plan: PlanKind::CaptureAll,
            branch: coords.branch,
            baseline_commit_sha: coords.baseline_commit_sha,
            head_commit_sha: coords.head_commit_sha,
            story_ids: None,
            reason: Some(reason),
            notes,
            build_id: coords.build_id,
        }
    }

    /// 選択結果から計画を組む。
    pub fn from_selection(coords: PlanCoordinates, selection: Selection) -> Self {
        match selection {
            Selection::CaptureAll { reason, notes } => Self::capture_all(coords, reason, notes),
            Selection::Only { story_ids, notes } => Self {
                version: PLAN_VERSION,
                plan: PlanKind::Only,
                branch: coords.branch,
                baseline_commit_sha: coords.baseline_commit_sha,
                head_commit_sha: coords.head_commit_sha,
                story_ids: Some(story_ids),
                reason: None,
                notes,
                build_id: coords.build_id,
            },
        }
    }

    /// CI が読む JSON（末尾改行なし）。
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("failed to serialize the selection plan")
    }
}
