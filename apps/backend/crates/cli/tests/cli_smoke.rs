//! `vrt` バイナリの CLI レベル smoke テスト。
//!
//! lib 経由の fixture テストとは別に、実際の引数解析・終了コード・stdout 契約を固定する。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

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

fn git_in(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(
        status.success(),
        "`git {}` failed in {}",
        args.join(" "),
        dir.display()
    );
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "`git {}` failed in {}",
        args.join(" "),
        dir.display()
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// 3 段の線形履歴を持つ一時 git リポジトリ。
fn init_linear_repo() -> (TempDir, String, String, String) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git_in(root, &["init", "-b", "main"]);
    git_in(root, &["config", "user.email", "vrt@test.local"]);
    git_in(root, &["config", "user.name", "vrt test"]);

    fs::write(root.join("README.md"), "base\n").expect("write README");
    git_in(root, &["add", "README.md"]);
    git_in(root, &["commit", "-m", "c1"]);
    let c1 = git_output(root, &["rev-parse", "HEAD"]);

    fs::write(root.join("README.md"), "second\n").expect("write README");
    git_in(root, &["add", "README.md"]);
    git_in(root, &["commit", "-m", "c2"]);
    let c2 = git_output(root, &["rev-parse", "HEAD"]);

    fs::write(root.join("README.md"), "third\n").expect("write README");
    git_in(root, &["add", "README.md"]);
    git_in(root, &["commit", "-m", "c3"]);
    let c3 = git_output(root, &["rev-parse", "HEAD"]);

    (tmp, c1, c2, c3)
}

fn git_rev_parse(repo: &Path, rev: &str) -> String {
    git_rev_parse_opt(repo, rev).unwrap_or_else(|| panic!("git rev-parse {rev} failed"))
}

fn git_rev_parse_opt(repo: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
        .current_dir(repo)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_full_oid(sha: &str) -> bool {
    sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())
}

#[test]
fn plan_without_credentials_exits_2() {
    let repo = repo_root();
    let head = git_rev_parse(&repo, "HEAD");
    let output = vrt()
        .args([
            "plan",
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            &head,
        ])
        .current_dir(&repo)
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
    // 開発中の実リポジトリは worktree が汚れていることがあるので、
    // クリーンな一時リポジトリで回す（絞り込み時は clean worktree が前提条件）。
    let (tmp, _c1, _c2, head) = init_linear_repo();
    let repo = tmp.path().to_path_buf();
    let baseline = head.clone();

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
            &head,
        ])
        .current_dir(&repo)
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
    assert!(is_full_oid(
        value["baseline_commit_sha"].as_str().expect("baseline")
    ));
    assert!(is_full_oid(
        value["head_commit_sha"].as_str().expect("head")
    ));
}

#[test]
fn plan_with_missing_baseline_commit_exits_2() {
    let repo = repo_root();
    let head = git_rev_parse(&repo, "HEAD");
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
            &head,
        ])
        .current_dir(&repo)
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

#[test]
fn plan_normalizes_baseline_ref_to_full_oid() {
    let (tmp, _c1, c2, head) = init_linear_repo();
    let repo = tmp.path();

    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            "HEAD~1",
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            &head,
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(value["baseline_commit_sha"].as_str().expect("baseline"), c2);
    assert_eq!(value["head_commit_sha"].as_str().expect("head"), head);
}

/// 絞り込み時、worktree は `--commit` の内容と一致していなければならない。
///
/// 選別の入力（stats / index）は worktree のファイルから読むため、diff の終点だけを
/// `--commit` に固定しても、worktree が別コミットなら「別内容に対する計画」になる。
/// この不一致は全撮影へ倒さず終了コード 2 のエラーにする（設定ミスを黙って読み替えない）。
///
/// positive control: 終点だけ固定して成功していた旧実装ではこの状況で exit 0 になり、
/// このテストは落ちる。
#[test]
fn plan_diff_uses_explicit_commit_not_worktree_head() {
    let (tmp, c1, c2, _head) = init_linear_repo();
    let repo = tmp.path();

    // worktree は c3（head）のまま、--commit は c2 → 前提不一致でエラー。
    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            &c1,
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            &c2,
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert_eq!(
        output.status.code(),
        Some(2),
        "worktree/commit mismatch must fail, not narrow against the wrong content; stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.is_empty(),
        "no plan may be emitted on a precondition failure"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not match the recorded commit"),
        "stderr={stderr}"
    );
}

/// worktree を `--commit` に合わせてあれば絞り込みは成立する（過剰ブロックの回帰防止）。
#[test]
fn plan_narrows_when_worktree_matches_the_recorded_commit() {
    let (tmp, c1, c2, _head) = init_linear_repo();
    let repo = tmp.path();
    git_in(repo, &["checkout", "--detach", &c2]);

    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            &c1,
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            &c2,
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(value["baseline_commit_sha"].as_str().expect("baseline"), c1);
    assert_eq!(value["head_commit_sha"].as_str().expect("head"), c2);
}

/// 追跡ファイルが汚れた worktree での絞り込みもエラー（stats/index が commit と別内容になりうる）。
#[test]
fn plan_rejects_a_dirty_tracked_worktree() {
    let (tmp, c1, _c2, head) = init_linear_repo();
    let repo = tmp.path();
    fs::write(repo.join("README.md"), "dirty\n").expect("dirty tracked file");

    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            &c1,
            "--dir",
            graph_fixture().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            &head,
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("uncommitted changes"), "stderr={stderr}");
}
