//! `vrt` バイナリの CLI レベル smoke テスト。
//!
//! lib 経由の fixture テストとは別に、実際の引数解析・終了コード・stdout 契約を固定する。

use std::path::{Path, PathBuf};
use std::process::Command;

fn vrt() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vrt"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .expect("repo root")
}

fn graph_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/graph")
}

#[test]
fn plan_without_credentials_exits_2() {
    let output = vrt()
        .args([
            "plan",
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            "head222",
        ])
        .current_dir(repo_root())
        .output()
        .expect("spawn vrt");

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty on pre-plan failure"
    );
}

#[test]
fn plan_with_baseline_commit_writes_json_to_stdout() {
    let baseline = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root())
        .output()
        .expect("git rev-parse");
    assert!(baseline.status.success());
    let baseline = String::from_utf8_lossy(&baseline.stdout).trim().to_string();

    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            &baseline,
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            "head222",
        ])
        .current_dir(repo_root())
        .output()
        .expect("spawn vrt");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    assert_eq!(value["version"], 1);
    assert!(value.get("plan").is_some());
    assert!(value.get("build_id").is_none());
}

#[test]
fn plan_with_missing_baseline_commit_exits_2() {
    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            "head222",
        ])
        .current_dir(repo_root())
        .output()
        .expect("spawn vrt");

    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid SHA must not fall back to capture_all; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty when baseline validation fails"
    );
}
