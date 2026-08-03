//! git との橋渡し。
//!
//! libgit2 系は使わず `git` バイナリを subprocess で呼ぶ（CI に git は必ずある、
//! かつ shallow clone 等の状態も git 本体の判断にそのまま乗れるため）。

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// `git <args>` を実行し、成功なら stdout を生バイト列のまま返す。
///
/// NUL 区切り出力（`-z`）用に trim も UTF-8 変換もしない。改行や空白を含む
/// パス名をそのまま受け取る唯一の経路なので、加工はすべて呼び出し側で行う。
fn git_raw(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `git {}`", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`git {}` failed: {}", args.join(" "), stderr.trim());
    }
    Ok(output.stdout)
}

/// `git <args>` を実行し、成功なら stdout を trim して返す。
fn git(args: &[&str]) -> Result<String> {
    let stdout = git_raw(args)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

/// リポジトリルート（`git rev-parse --show-toplevel`）。
pub fn repo_root() -> Result<PathBuf> {
    let out = git(&["rev-parse", "--show-toplevel"])
        .context("could not determine the git repository root; run inside a git checkout")?;
    Ok(PathBuf::from(out))
}

/// 現在のブランチ名。detached HEAD なら `HEAD` を返すので、その場合はエラーにする。
pub fn current_branch() -> Result<String> {
    let name = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    if name == "HEAD" {
        bail!("detached HEAD: pass --branch explicitly");
    }
    Ok(name)
}

/// リビジョンを完全な commit OID へ正規化する（`git rev-parse --verify <rev>^{commit}`）。
///
/// 存在確認も兼ねる。`main` や `HEAD~1` などの ref 名も受け付ける。
pub fn resolve_commit(rev: &str) -> Result<String> {
    git(&[
        "rev-parse",
        "--verify",
        "--end-of-options",
        &format!("{rev}^{{commit}}"),
    ])
    .with_context(|| format!("could not resolve commit {rev}"))
}

/// 現在の commit SHA（`git rev-parse HEAD`）。
pub fn head_commit() -> Result<String> {
    resolve_commit("HEAD")
}

/// 絞り込み（差分選別）の前提を検証する。
///
/// stats / index は worktree のファイルから読む。選別は「`commit` までの差分」を
/// 前提に組まれるので、worktree の内容が `commit` と一致していなければ、
/// 差分の終点と選別の入力（依存グラフ・ストーリー一覧）がずれた計画になる。
/// 全撮影へ黙って倒すと設定ミス（別コミットの成果物を渡した等）に気づけないため、
/// 満たさない場合はエラーにする。
///
/// - worktree の `HEAD` が `commit` と一致しない → エラー
/// - 追跡ファイルに未コミットの変更がある（`git status --porcelain -uno`）→ エラー
///   （未追跡ファイルは stats に現れないため対象外）
pub fn verify_worktree_matches(commit: &str) -> Result<()> {
    let head = head_commit()?;
    if head != commit {
        bail!(
            "worktree HEAD ({head}) does not match the recorded commit ({commit}); \
             check out that exact commit before narrowing the capture set"
        );
    }
    let status = git(&["status", "--porcelain", "--untracked-files=no"])?;
    if !status.is_empty() {
        bail!(
            "tracked files have uncommitted changes, so the stats/index in the worktree \
             may not correspond to commit {commit}:\n{status}"
        );
    }
    Ok(())
}

/// `from_commit` から `to_commit` までの変更ファイル一覧（リポジトリルート相対）。
///
/// 両端は [`resolve_commit`] で正規化してから `git diff --name-only` する。
/// 計画 JSON の `baseline_commit_sha` / `head_commit_sha` と同じ 2 点間の差分を
/// 選別に使うため、呼び出し側が渡した head と常に一致させる。
///
/// 出力は `-z`（NUL 区切り・パス名を quoting せず生のまま出す）で受ける。
/// 既定の `core.quotepath=true` は非 ASCII / `"` / `\` 入りのパスを
/// `"\346\227\245..."` 形式の C-quoting で出力し、その文字列は依存グラフの
/// キー（stats JSON 由来の生パス）と一致しないため、非 ASCII パスの変更が
/// 常に「グラフ外 → capture_all」へ倒れて差分選別が無効化されていた。
/// `-c core.quotepath=false` は、将来 `-z` が外れても quoting へ戻らないための
/// 保険として併置する。行区切り + trim のパースは改行・前後空白入りの
/// パス名を壊すため使わない。
pub fn changed_files(from_commit: &str, to_commit: &str) -> Result<Vec<String>> {
    let from = resolve_commit(from_commit)?;
    let to = resolve_commit(to_commit)?;

    let out = git_raw(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--name-only",
        "-z",
        &from,
        &to,
        "--",
    ])?;
    Ok(out
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        // グラフのキーは stats JSON 由来で常に有効な UTF-8。UTF-8 でない
        // パスはどのみちキーに一致せず capture_all へ倒れるため lossy でよい。
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git_in, git_output, init_test_repo};
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static REPO_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct ChdirGuard {
        previous: PathBuf,
    }

    impl ChdirGuard {
        fn change_to(dir: &Path) -> Self {
            let previous = std::env::current_dir().expect("cwd");
            std::env::set_current_dir(dir).expect("chdir");
            Self { previous }
        }
    }

    impl Drop for ChdirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    /// 3 段の線形履歴（c1 → c2 で a.txt、c2 → c3 で b.txt）。
    fn init_linear_repo() -> (TempDir, String, String, String) {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_test_repo(root);

        fs::write(root.join("a.txt"), "a1\n").expect("write a.txt");
        git_in(root, &["add", "a.txt"]);
        git_in(root, &["commit", "-m", "c1"]);
        let c1 = git_output(root, &["rev-parse", "HEAD"]);

        fs::write(root.join("a.txt"), "a2\n").expect("write a.txt");
        git_in(root, &["add", "a.txt"]);
        git_in(root, &["commit", "-m", "c2"]);
        let c2 = git_output(root, &["rev-parse", "HEAD"]);

        fs::write(root.join("b.txt"), "b1\n").expect("write b.txt");
        git_in(root, &["add", "b.txt"]);
        git_in(root, &["commit", "-m", "c3"]);
        let c3 = git_output(root, &["rev-parse", "HEAD"]);

        (tmp, c1, c2, c3)
    }

    #[test]
    fn resolve_commit_normalizes_head_ref() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, _c1, _c2, c3) = init_linear_repo();
        let _guard = ChdirGuard::change_to(tmp.path());
        let oid = resolve_commit("HEAD").expect("HEAD");
        assert_eq!(oid, c3);
        assert_eq!(oid.len(), 40);
        assert!(oid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_commit_accepts_head_tilde_notation() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, _c1, c2, c3) = init_linear_repo();
        let _guard = ChdirGuard::change_to(tmp.path());
        let head = resolve_commit("HEAD").expect("HEAD");
        let parent = resolve_commit("HEAD~1").expect("HEAD~1");
        assert_eq!(head, c3);
        assert_eq!(parent, c2);
        assert_ne!(head, parent);
    }

    #[test]
    fn changed_files_diffs_between_two_explicit_commits() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, c1, c2, _c3) = init_linear_repo();
        let _guard = ChdirGuard::change_to(tmp.path());
        let files = changed_files(&c1, &c2).expect("diff");
        assert_eq!(files, vec!["a.txt".to_string()]);
    }

    /// 非 ASCII パス: 既定の `core.quotepath=true` では `git diff --name-only` が
    /// `"\346..."` 形式の C-quoting で出力し、実ファイル名と一致しない
    /// （= 依存グラフのキーに一致せず常に capture_all へ倒れる）。
    /// 前半がその壊れ方を固定する positive control で、行ベース + trim の
    /// 修正前 `changed_files` はこの quoted 文字列をそのまま返すため後半の
    /// assert が落ちる。
    #[test]
    fn changed_files_returns_non_ascii_paths_verbatim() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_test_repo(root);

        fs::write(root.join("base.txt"), "base\n").expect("write base.txt");
        git_in(root, &["add", "base.txt"]);
        git_in(root, &["commit", "-m", "c1"]);
        let c1 = git_output(root, &["rev-parse", "HEAD"]);

        let name = "部品/ボタン.stories.tsx";
        fs::create_dir_all(root.join("部品")).expect("mkdir 部品");
        fs::write(root.join(name), "export {}\n").expect("write non-ascii file");
        git_in(root, &["add", "."]);
        git_in(root, &["commit", "-m", "c2"]);
        let c2 = git_output(root, &["rev-parse", "HEAD"]);

        // positive control: git の既定（quotepath=true）は C-quoting で出力し、
        // 実ファイル名とは一致しない。環境の gitconfig に依存しないよう明示指定。
        let quoted = git_output(
            root,
            &["-c", "core.quotepath=true", "diff", "--name-only", &c1, &c2],
        );
        assert_ne!(
            quoted, name,
            "with the default quotepath, git must C-quote the path (this is the bug \
             changed_files has to undo)"
        );
        assert!(
            quoted.starts_with('"') && quoted.contains("\\3"),
            "expected C-quoted octal escapes, got {quoted:?}"
        );

        let _guard = ChdirGuard::change_to(root);
        let files = changed_files(&c1, &c2).expect("diff");
        assert_eq!(files, vec![name.to_string()]);
    }

    /// 改行入りファイル名: 行区切りパース（修正前）は 1 ファイルを 2 つの
    /// 存在しないパスへ割ってしまう positive control。NUL 区切りなら 1 件の
    /// まま原文で返る。
    #[test]
    fn changed_files_preserves_newline_in_filename() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_test_repo(root);

        fs::write(root.join("base.txt"), "base\n").expect("write base.txt");
        git_in(root, &["add", "base.txt"]);
        git_in(root, &["commit", "-m", "c1"]);
        let c1 = git_output(root, &["rev-parse", "HEAD"]);

        let name = "odd\nname.txt";
        fs::write(root.join(name), "x\n").expect("write newline-named file");
        git_in(root, &["add", "."]);
        git_in(root, &["commit", "-m", "c2"]);
        let c2 = git_output(root, &["rev-parse", "HEAD"]);

        let _guard = ChdirGuard::change_to(root);
        let files = changed_files(&c1, &c2).expect("diff");
        assert_eq!(files, vec![name.to_string()]);
    }

    /// ハイフン始まりの ref は `--end-of-options` 無しだと git がオプション解釈する。
    /// 修正前の `resolve_commit` はここで失敗する positive control。
    #[test]
    fn resolve_commit_accepts_hyphen_prefixed_ref() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, _c1, _c2, c3) = init_linear_repo();
        let root = tmp.path();
        git_in(root, &["update-ref", "refs/heads/-hyphen-branch", &c3]);

        let without_eoo = Command::new("git")
            .args(["rev-parse", "--verify", "-hyphen-branch^{commit}"])
            .current_dir(root)
            .output()
            .expect("spawn git");
        assert!(
            !without_eoo.status.success(),
            "without --end-of-options, hyphen-prefixed rev must not resolve; stderr={}",
            String::from_utf8_lossy(&without_eoo.stderr)
        );

        let _guard = ChdirGuard::change_to(root);
        let oid = resolve_commit("-hyphen-branch").expect("hyphen-prefixed ref");
        assert_eq!(oid, c3);
        assert_eq!(oid.len(), 40);
    }
}
