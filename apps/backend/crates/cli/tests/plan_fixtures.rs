//! `screenshots` モードの選択計画の統合テスト（fixture ベース）。
//!
//! `tests/fixtures/plan/` 配下の stats / index を実ファイルとして読ませ、
//!
//! - 到達した story だけが選ばれること
//! - 到達 0 件が「空の選択」であり全撮影と区別されること
//! - stats / index が無い・壊れているときに全撮影へ倒れること
//! - `storybook` モード（`CorruptInput::Error`）の既存挙動が変わらないこと
//! - CI へ渡す JSON の形が契約どおりであること
//!
//! を固定する。

use std::path::{Path, PathBuf};

use vrt_cli::plan::{
    self, CorruptInput, PLAN_VERSION, PlanCoordinates, PlanDocument, PlanKind, Selection,
    SelectionInputs,
};

/// storybook build を apps/frontend で回した想定。stats のモジュール名は cwd 相対。
const REPO_ROOT: &str = "/repo";
const CWD: &str = "/repo/apps/frontend";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan")
        .join(name)
}

fn select(
    fixture_name: &str,
    changed: &[&str],
    on_corrupt: CorruptInput,
) -> anyhow::Result<Selection> {
    let dir = fixture(fixture_name);
    let changed: Vec<String> = changed.iter().map(|s| s.to_string()).collect();
    plan::select_stories(
        &SelectionInputs {
            dir: &dir,
            stats_json: None,
            index_json: None,
            repo_root: Path::new(REPO_ROOT),
            cwd: Path::new(CWD),
            changed_files: &changed,
        },
        on_corrupt,
    )
}

/// screenshots（fail-closed）で選択する。読み込み失敗もエラーにならない。
fn plan_selection(fixture_name: &str, changed: &[&str]) -> Selection {
    select(fixture_name, changed, CorruptInput::FailClosed).expect("fail-closed never errors")
}

fn story_ids(selection: &Selection) -> Vec<String> {
    match selection {
        Selection::Only { story_ids, .. } => story_ids.clone(),
        Selection::CaptureAll { reason, .. } => {
            panic!("expected a story set, got capture-all: {reason}")
        }
    }
}

// ── ①「撮らない」の選択 ────────────────────────────────────────────────

#[test]
fn change_selects_only_reachable_stories() {
    // A.tsx を変更 → A の story 2 件だけ。B へは届かない。
    let selection = plan_selection("graph", &["apps/frontend/src/A.tsx"]);
    assert_eq!(story_ids(&selection), vec!["a--one", "a--two"]);
}

#[test]
fn shared_dependency_reaches_every_dependent_story() {
    let selection = plan_selection("graph", &["apps/frontend/src/util/format.ts"]);
    assert_eq!(story_ids(&selection), vec!["a--one", "a--two", "b--one"]);
}

#[test]
fn reaching_no_story_is_an_empty_set_not_capture_all() {
    // グラフ上にはあるが、どの story からも import されていないモジュール。
    // 「撮るものが無い」は選択結果であり、全撮影ではない。
    let selection = plan_selection("graph", &["apps/frontend/src/orphan/Helper.ts"]);
    assert_eq!(story_ids(&selection), Vec::<String>::new());
}

#[test]
fn docs_entries_are_never_selected() {
    let selection = plan_selection("graph", &["apps/frontend/src/util/format.ts"]);
    assert!(!story_ids(&selection).iter().any(|id| id == "intro--docs"));
}

// ── fail-closed（全撮影へ倒す） ─────────────────────────────────────────

#[test]
fn dependency_update_falls_back_to_capture_all() {
    let selection = plan_selection("graph", &["pnpm-lock.yaml"]);
    assert!(matches!(selection, Selection::CaptureAll { .. }));
}

#[test]
fn change_outside_the_graph_falls_back_to_capture_all() {
    let selection = plan_selection("graph", &["apps/frontend/src/New.tsx"]);
    assert!(matches!(selection, Selection::CaptureAll { .. }));
}

#[test]
fn missing_stats_falls_back_to_capture_all() {
    let selection = plan_selection("no-stats", &["apps/frontend/src/A.tsx"]);
    match selection {
        Selection::CaptureAll { reason, .. } => {
            assert!(reason.contains("preview-stats.json"), "reason: {reason}");
            // 差分撮影を有効にする手順まで理由に残す。
            assert!(reason.contains("--stats-json"), "reason: {reason}");
        }
        Selection::Only { .. } => panic!("missing stats must fall back to capture-all"),
    }
}

#[test]
fn corrupt_stats_falls_back_to_capture_all() {
    let selection = plan_selection("corrupt-stats", &["apps/frontend/src/A.tsx"]);
    match selection {
        Selection::CaptureAll { reason, .. } => {
            assert!(reason.contains("preview-stats.json"), "reason: {reason}")
        }
        Selection::Only { .. } => panic!("corrupt stats must fall back to capture-all"),
    }
}

#[test]
fn corrupt_index_falls_back_to_capture_all() {
    let selection = plan_selection("corrupt-index", &["apps/frontend/src/A.tsx"]);
    match selection {
        Selection::CaptureAll { reason, .. } => {
            assert!(reason.contains("index.json"), "reason: {reason}")
        }
        Selection::Only { .. } => panic!("corrupt index must fall back to capture-all"),
    }
}

// ── storybook モードの既存挙動の固定 ───────────────────────────────────

#[test]
fn storybook_mode_selects_the_same_story_set() {
    // 健全な入力では、両モードの選択結果は一致する（選択器を二重に持たない）。
    let screenshots = plan_selection("graph", &["apps/frontend/src/A.tsx"]);
    let storybook = select("graph", &["apps/frontend/src/A.tsx"], CorruptInput::Error)
        .expect("healthy input never errors");
    assert_eq!(screenshots, storybook);
}

#[test]
fn storybook_mode_still_falls_back_when_stats_are_absent() {
    // stats 不在は従来どおり全撮影（エラーではない）。
    let selection = select(
        "no-stats",
        &["apps/frontend/src/A.tsx"],
        CorruptInput::Error,
    )
    .expect("absent stats is not an error");
    assert!(matches!(selection, Selection::CaptureAll { .. }));
}

#[test]
fn storybook_mode_still_errors_on_corrupt_stats() {
    // 壊れた stats を黙って全撮影へ読み替えると設定ミスに気づけない。
    let err = select(
        "corrupt-stats",
        &["apps/frontend/src/A.tsx"],
        CorruptInput::Error,
    )
    .expect_err("corrupt stats must stay an error in storybook mode");
    assert!(format!("{err:#}").contains("preview-stats.json"));
}

#[test]
fn storybook_mode_still_errors_on_corrupt_index() {
    let err = select(
        "corrupt-index",
        &["apps/frontend/src/A.tsx"],
        CorruptInput::Error,
    )
    .expect_err("corrupt index must stay an error in storybook mode");
    assert!(format!("{err:#}").contains("index.json"));
}

// ── CI へ渡す JSON 契約 ────────────────────────────────────────────────

fn coords() -> PlanCoordinates {
    PlanCoordinates {
        branch: "feat/x".to_string(),
        baseline_commit_sha: Some("base111".to_string()),
        head_commit_sha: "head222".to_string(),
        build_id: Some("build-uuid".to_string()),
    }
}

fn as_value(document: &PlanDocument) -> serde_json::Value {
    serde_json::from_str(&document.to_json().expect("serialize")).expect("valid JSON")
}

#[test]
fn only_plan_carries_the_story_set_and_pinned_commits() {
    let document = PlanDocument::from_selection(
        coords(),
        plan_selection("graph", &["apps/frontend/src/A.tsx"]),
    );
    let value = as_value(&document);
    assert_eq!(value["version"], PLAN_VERSION);
    assert_eq!(value["plan"], "only");
    assert_eq!(value["story_ids"][0], "a--one");
    assert_eq!(value["story_ids"][1], "a--two");
    // 計画は baseline と HEAD を固定して持つ（CI 側で変化を検知させる）。
    assert_eq!(value["baseline_commit_sha"], "base111");
    assert_eq!(value["head_commit_sha"], "head222");
    assert_eq!(value["build_id"], "build-uuid");
    assert!(value["reason"].is_null());
}

#[test]
fn empty_only_plan_is_distinguishable_from_capture_all() {
    let document = PlanDocument::from_selection(
        coords(),
        plan_selection("graph", &["apps/frontend/src/orphan/Helper.ts"]),
    );
    let value = as_value(&document);
    // 空集合は「撮るものが無い」。plan は only のまま。
    assert_eq!(value["plan"], "only");
    assert_eq!(value["story_ids"].as_array().expect("array").len(), 0);
    assert_eq!(document.plan, PlanKind::Only);
}

#[test]
fn capture_all_plan_omits_story_ids_and_keeps_the_reason() {
    let document =
        PlanDocument::from_selection(coords(), plan_selection("graph", &["pnpm-lock.yaml"]));
    let value = as_value(&document);
    assert_eq!(value["plan"], "capture_all");
    assert!(
        value.get("story_ids").is_none(),
        "story_ids must be omitted"
    );
    assert!(
        value["reason"]
            .as_str()
            .expect("reason")
            .contains("pnpm-lock.yaml"),
        "reason: {}",
        value["reason"]
    );
}

#[test]
fn capture_all_without_baseline_records_the_missing_baseline() {
    let document = PlanDocument::capture_all(
        PlanCoordinates {
            branch: "feat/x".to_string(),
            baseline_commit_sha: None,
            head_commit_sha: "head222".to_string(),
            build_id: None,
        },
        "no baseline commit for this branch yet".to_string(),
        Vec::new(),
    );
    let value = as_value(&document);
    assert_eq!(value["plan"], "capture_all");
    assert!(value["baseline_commit_sha"].is_null());
    assert!(value.get("build_id").is_none());
}

#[test]
fn explicit_missing_baseline_commit_is_not_capture_all() {
    let err = vrt_cli::git::commit_exists("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        .expect_err("missing SHA must error");
    assert!(
        err.to_string().contains("not present locally"),
        "err={err:#}"
    );
}

#[test]
fn notes_survive_into_the_plan() {
    // グラフ外でも .md は無視される。その判断は notes に残す。
    let selection = plan_selection("graph", &["README.md", "apps/frontend/src/A.tsx"]);
    let document = PlanDocument::from_selection(coords(), selection);
    assert!(
        document.notes.iter().any(|n| n.contains("README.md")),
        "notes: {:?}",
        document.notes
    );
}

#[test]
fn plan_document_round_trips_through_json() {
    let document = PlanDocument::from_selection(
        coords(),
        plan_selection("graph", &["apps/frontend/src/A.tsx"]),
    );
    let json = document.to_json().expect("serialize");
    let parsed: PlanDocument = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, document);
}
