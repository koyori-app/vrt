//! TurboSnap 影響算出の統合テスト（fixture ベース）。
//!
//! webpack stats と index.json の小さな JSON を組み立て、変更ファイルの各ケース
//! （story 到達 / 非到達 / lockfile トリガー / グラフ外）で撮影対象が正しく
//! 絞られる/全撮影に倒れることを、公開 API 経由で確認する。

use std::path::Path;

use vrt_cli::turbosnap::{self, Plan, WebpackStats};

/// A.tsx → A.stories.tsx、共有 utils.ts → A/B 両方、という多段グラフ。
const STATS: &str = r#"{
  "modules": [
    { "name": "./src/util/format.ts", "reasons": [
        { "moduleName": "./src/A.tsx" },
        { "moduleName": "./src/B.tsx" }
    ]},
    { "name": "./src/A.tsx", "reasons": [ { "moduleName": "./src/A.stories.tsx" } ] },
    { "name": "./src/B.tsx", "reasons": [ { "moduleName": "./src/B.stories.tsx" } ] },
    { "name": "./src/A.stories.tsx", "reasons": [] },
    { "name": "./src/B.stories.tsx", "reasons": [] }
  ]
}"#;

const INDEX: &str = r#"{
  "v": 5,
  "entries": {
    "a--one":  { "id": "a--one",  "type": "story", "importPath": "./src/A.stories.tsx" },
    "b--one":  { "id": "b--one",  "type": "story", "importPath": "./src/B.stories.tsx" }
  }
}"#;

fn run(changed: &[&str]) -> Plan {
    let repo_root = Path::new("/repo");
    let cwd = Path::new("/repo/apps/frontend");
    let stats = WebpackStats::parse(STATS).expect("stats");
    let stories = turbosnap::parse_index(INDEX).expect("index");
    let changed: Vec<String> = changed.iter().map(|s| s.to_string()).collect();
    turbosnap::compute_affected_stories(repo_root, cwd, &changed, &stats, &stories).plan
}

#[test]
fn leaf_change_reaches_single_story() {
    // A.tsx だけ変更 → A のストーリーのみ。
    assert_eq!(
        run(&["apps/frontend/src/A.tsx"]),
        Plan::Only(vec!["a--one".into()])
    );
}

#[test]
fn shared_dep_change_reaches_all_dependents() {
    // 共有 util を変更 → A/B 両方のストーリーへ波及する。
    assert_eq!(
        run(&["apps/frontend/src/util/format.ts"]),
        Plan::Only(vec!["a--one".into(), "b--one".into()])
    );
}

#[test]
fn unrelated_story_not_reached() {
    // B.tsx 変更で A は撮らない。
    assert_eq!(
        run(&["apps/frontend/src/B.tsx"]),
        Plan::Only(vec!["b--one".into()])
    );
}

#[test]
fn lockfile_forces_full_capture() {
    assert!(matches!(run(&["pnpm-lock.yaml"]), Plan::CaptureAll(_)));
}

#[test]
fn unknown_source_file_forces_full_capture() {
    assert!(matches!(
        run(&["apps/frontend/src/New.tsx"]),
        Plan::CaptureAll(_)
    ));
}
