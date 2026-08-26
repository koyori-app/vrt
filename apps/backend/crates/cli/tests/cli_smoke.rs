//! `vrt` バイナリの CLI レベル smoke テスト。
//!
//! lib 経由の fixture テストとは別に、実際の引数解析・終了コード・stdout 契約を固定する。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use vrt_cli::test_support::{git_in, git_output, init_test_repo};

fn vrt() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vrt"))
}

fn graph_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/graph")
}

/// 3 段の線形履歴を持つ一時 git リポジトリ。
fn init_linear_repo() -> (TempDir, String, String, String) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    init_test_repo(root);

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

/// graph fixture の依存グラフに対応する 2 段履歴。c1 → c2 で
/// `apps/frontend/src/A.tsx` を変更する（stats のモジュール名と揃えた配置）。
fn init_story_diff_repo() -> (TempDir, String, String) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    init_test_repo(root);

    let a_path = root.join("apps/frontend/src/A.tsx");
    let b_path = root.join("apps/frontend/src/B.tsx");
    fs::create_dir_all(a_path.parent().expect("parent")).expect("mkdir");
    fs::write(&a_path, "export const A = 1;\n").expect("write A");
    fs::write(&b_path, "export const B = 1;\n").expect("write B");
    git_in(root, &["add", "apps"]);
    git_in(root, &["commit", "-m", "c1"]);
    let c1 = git_output(root, &["rev-parse", "HEAD"]);

    fs::write(&a_path, "export const A = 2;\n").expect("write A");
    git_in(root, &["add", "apps/frontend/src/A.tsx"]);
    git_in(root, &["commit", "-m", "c2"]);
    let c2 = git_output(root, &["rev-parse", "HEAD"]);

    (tmp, c1, c2)
}

fn is_full_oid(sha: &str) -> bool {
    sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())
}

// この 2 テスト（credentials 無し / baseline 不在）は開発リポジトリの HEAD に
// 依存させず、一時リポジトリで自己完結させる。`git archive` で展開した
// .git 無しの配布ソースでも通ることを保証するため（検証手順は
// リポジトリ README「配布ソース（.git 無し）での検証」を参照）。
/// 値を省いた `--exit-zero-on-changes` が、後ろのフラグを値として飲み込まないこと。
/// 飲み込むと `--json` が消え、呼び出し元が結果 JSON を受け取れなくなる。
/// 引数解析を抜けた証拠として、ビルド作成の通信エラーまで進むことを見る。
#[test]
fn bare_exit_zero_on_changes_does_not_swallow_the_next_flag() {
    let (tmp, _c1, _c2, _head) = init_linear_repo();
    let output = vrt()
        .args([
            "upload",
            // 接続だけ失敗させたいので、閉じているポートを指す。
            "--url",
            "http://127.0.0.1:1",
            "--token",
            "t",
            "--project",
            "acme/web",
            "--exit-zero-on-changes",
            "--json",
        ])
        .current_dir(tmp.path())
        .output()
        .expect("spawn vrt");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("create build request failed"),
        "the bare flag must not consume --json; stderr={stderr}"
    );
}

/// PR 番号は 1 以上でなければ意味がないので、引数解析の段階で弾く。
/// 素通しすると不正な番号のまま create_build まで進み、失敗が CI の後半へずれる。
#[test]
fn upload_rejects_non_positive_pull_request_numbers() {
    for invalid in ["0", "-1"] {
        let output = vrt()
            .args([
                "upload",
                "--url",
                "http://127.0.0.1:1",
                "--token",
                "t",
                "--project",
                "acme/web",
                "--pull-request",
                invalid,
            ])
            .output()
            .expect("spawn vrt");

        assert!(
            !output.status.success(),
            "--pull-request {invalid} must be rejected, stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn plan_without_credentials_exits_2() {
    let (tmp, _c1, _c2, head) = init_linear_repo();
    let repo = tmp.path().to_path_buf();
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
    let (tmp, _c1, _c2, head) = init_linear_repo();
    let repo = tmp.path().to_path_buf();
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

/// stamp 用: リポジトリ内に成果物ディレクトリを掘り、fixture の stats / index を
/// 「build コマンドが生成する」シェルスクリプトを返す。
///
/// stamp は build 後に stats / index の存在を要求するため、build コマンド側で
/// 複製することで「vrt が build を実行してから stamp した」順序そのものを検証する。
fn copy_fixture_build_command(dest: &Path) -> String {
    let fixture = graph_fixture();
    format!(
        "mkdir -p '{dest}' && cp '{fix}/preview-stats.json' '{fix}/index.json' '{dest}/'",
        dest = dest.display(),
        fix = fixture.display()
    )
}

fn provenance_file(dir: &Path) -> PathBuf {
    dir.join("vrt-provenance.json")
}

/// `vrt stamp -- <build command>` の基本形: vrt が build を実行し、前後で観測した
/// HEAD を v2 provenance として書く。
#[test]
fn stamp_runs_the_build_and_writes_v2_provenance() {
    let (tmp, _c1, _c2, head) = init_linear_repo();
    let repo = tmp.path();
    let dest = repo.join("storybook-static");

    let output = vrt()
        .args([
            "stamp",
            "--dir",
            dest.to_str().expect("utf8"),
            "--",
            "sh",
            "-c",
            &copy_fixture_build_command(&dest),
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = fs::read_to_string(provenance_file(&dest)).expect("provenance written");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(value["version"], 2);
    assert_eq!(value["head_commit_sha"].as_str().expect("head"), head);
    assert_eq!(value["build_command"][0], "sh");
}

/// build コマンドが失敗したら stamp しない（証明の無い成果物を作らない）。
#[test]
fn stamp_does_not_stamp_when_the_build_fails() {
    let (tmp, _c1, _c2, _head) = init_linear_repo();
    let repo = tmp.path();
    let dest = repo.join("storybook-static");

    let output = vrt()
        .args([
            "stamp",
            "--dir",
            dest.to_str().expect("utf8"),
            "--",
            "sh",
            "-c",
            "exit 7",
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("build command"), "stderr={stderr}");
    assert!(!provenance_file(&dest).exists(), "nothing may be stamped");
}

/// build 中に HEAD が動いたら stamp しない——build 前後の HEAD 同一性こそが
/// 「その commit でビルドした」証明の中身だからである。
/// positive control: stamp 時点の HEAD しか見ない旧形（build 非所有）では
/// この状況を観測する計器が存在せず、検出のしようがなかった。
#[test]
fn stamp_rejects_a_build_that_moves_head() {
    let (tmp, _c1, _c2, _head) = init_linear_repo();
    let repo = tmp.path();
    let dest = repo.join("storybook-static");
    let build = format!(
        "{} && git commit --allow-empty -m moved",
        copy_fixture_build_command(&dest)
    );

    let output = vrt()
        .args([
            "stamp",
            "--dir",
            dest.to_str().expect("utf8"),
            "--",
            "sh",
            "-c",
            &build,
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("HEAD moved during the build"),
        "stderr={stderr}"
    );
    assert!(!provenance_file(&dest).exists(), "nothing may be stamped");
}

/// build が追跡ファイルを書き換えたら stamp しない（成果物を HEAD へ帰属できない）。
#[test]
fn stamp_rejects_a_build_that_dirties_tracked_files() {
    let (tmp, _c1, _c2, _head) = init_linear_repo();
    let repo = tmp.path();
    let dest = repo.join("storybook-static");
    let build = format!(
        "{} && printf dirty >> README.md",
        copy_fixture_build_command(&dest)
    );

    let output = vrt()
        .args([
            "stamp",
            "--dir",
            dest.to_str().expect("utf8"),
            "--",
            "sh",
            "-c",
            &build,
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dirty"), "stderr={stderr}");
    assert!(!provenance_file(&dest).exists(), "nothing may be stamped");
}

/// build コマンド無しの stamp は受け付けない（build 後の後追い stamp の口を
/// CLI から消す）。clap の usage エラーで終了コード 2。
#[test]
fn stamp_without_a_build_command_is_a_usage_error() {
    let (tmp, _c1, _c2, _head) = init_linear_repo();
    let repo = tmp.path();

    let output = vrt()
        .args(["stamp", "--dir", "storybook-static"])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert!(!output.status.success());
    assert!(!provenance_file(&repo.join("storybook-static")).exists());
}

/// キャッシュ復元シナリオの端から端まで: c2 で vrt が build を所有して stamp した
/// 成果物を、c3 の checkout（キャッシュ復元で古い成果物を掴んだ状況）で plan に
/// 使うと、計画は出ず終了コード 2 で落ちる。
/// positive control: provenance を見ない実装では c3 の diff に c2 の stats を
/// 混ぜた計画が exit 0 で出てしまう（修正前は通る）。
#[test]
fn plan_rejects_a_cache_restored_artifact_built_at_an_older_commit() {
    let (tmp, c1, c2, c3) = init_linear_repo();
    let repo = tmp.path();
    let dest = repo.join("storybook-static");

    // c2 を checkout し、vrt が build を所有して stamp（正しい生成手順）。
    git_in(repo, &["checkout", "--detach", &c2]);
    let output = vrt()
        .args([
            "stamp",
            "--dir",
            dest.to_str().expect("utf8"),
            "--",
            "sh",
            "-c",
            &copy_fixture_build_command(&dest),
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // c3 へ進む（キャッシュから古い成果物を復元した CI の再現）。
    git_in(repo, &["checkout", "--detach", &c3]);
    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            &c1,
            "--dir",
            dest.to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            &c3,
        ])
        .current_dir(repo)
        .output()
        .expect("spawn vrt");

    assert_eq!(
        output.status.code(),
        Some(2),
        "a stale artifact must not narrow; stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty(), "no plan may be emitted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("built from commit"), "stderr={stderr}");
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

/// worktree が `--commit` と一致していてもエラーにはならない（過剰ブロックの
/// 回帰防止）が、fixture に provenance が無いため計画は全撮影へ倒れる。
///
/// かつてこのテストは SHA 2 つしか assert しておらず、名前に反して
/// 「絞り込めたか」を見ていなかった（provenance 無しでは capture_all になる）。
/// 実際に絞り込める連鎖は [`a_stamped_artifact_narrows_the_plan_to_affected_stories`]
/// が固定する。
#[test]
fn plan_without_provenance_falls_back_to_capture_all_even_when_worktree_matches() {
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
    // provenance の無い成果物では絞り込まない（撮り逃しを作らない側へ倒す）。
    assert_eq!(value["plan"].as_str(), Some("capture_all"), "value={value}");
    assert!(
        value["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("provenance"),
        "the fallback reason must point at the missing provenance: {value}"
    );
}

/// `stamp → plan` の成功連鎖の smoke: stamp した成果物なら計画は
/// `plan = "only"` に絞られ、変更の影響を受けた story だけが載る。
///
/// positive control: 絞り込みが成立しない実装（provenance 検証や選択が
/// 壊れて capture_all に倒れる等）では `plan = "only"` のアサートで落ちる。
#[test]
fn a_stamped_artifact_narrows_the_plan_to_affected_stories() {
    let (tmp, c1, c2) = init_story_diff_repo();
    let repo = tmp.path();
    git_in(repo, &["checkout", "--detach", &c2]);
    let frontend = repo.join("apps/frontend");

    // 成果物は stamp の build コマンドが再生成する（後追い stamp は受け付けない）。
    // fixture はリポジトリ内の読み取り専用ファイルなので、複製元を backup に置き、
    // build コマンドにコピーさせる。
    let backup = TempDir::new().expect("backup tempdir");
    for name in ["preview-stats.json", "index.json"] {
        fs::copy(graph_fixture().join(name), backup.path().join(name)).expect("copy fixture");
    }
    let artifact = TempDir::new().expect("artifact tempdir");
    let script = format!(
        "cp '{b}/preview-stats.json' '{b}/index.json' '{a}/'",
        b = backup.path().display(),
        a = artifact.path().display()
    );
    let output = vrt()
        .args([
            "stamp",
            "--dir",
            artifact.path().to_str().expect("utf8"),
            "--",
            "sh",
            "-c",
            &script,
        ])
        .current_dir(&frontend)
        .output()
        .expect("spawn vrt stamp");
    assert!(
        output.status.success(),
        "stamp stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // c1 → c2 の変更は A.tsx。graph fixture では A の story 2 件に届く。
    let output = vrt()
        .args([
            "plan",
            "--baseline-commit",
            &c1,
            "--dir",
            artifact.path().to_str().expect("utf8"),
            "--branch",
            "feat/test",
            "--commit",
            &c2,
        ])
        .current_dir(&frontend)
        .output()
        .expect("spawn vrt plan");
    assert!(
        output.status.success(),
        "plan stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(value["plan"].as_str(), Some("only"), "value={value}");
    assert_eq!(
        value["story_ids"],
        serde_json::json!(["a--one", "a--two"]),
        "only the stories reachable from the changed file are selected: {value}"
    );
    assert_eq!(value["reason"], serde_json::Value::Null);
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
