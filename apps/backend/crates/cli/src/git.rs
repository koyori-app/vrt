//! git との橋渡し。
//!
//! libgit2 系は使わず `git` バイナリを subprocess で呼ぶ（CI に git は必ずある、
//! かつ shallow clone 等の状態も git 本体の判断にそのまま乗れるため）。

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// `git <args>` を実行し、成功なら stdout を trim して返す。
fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `git {}`", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`git {}` failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

/// `from_commit` から `to_commit` までの変更ファイル一覧（リポジトリルート相対）。
///
/// 両端は [`resolve_commit`] で正規化してから `git diff --name-only` する。
/// 計画 JSON の `baseline_commit_sha` / `head_commit_sha` と同じ 2 点間の差分を
/// 選別に使うため、呼び出し側が渡した head と常に一致させる。
pub fn changed_files(from_commit: &str, to_commit: &str) -> Result<Vec<String>> {
    let from = resolve_commit(from_commit)?;
    let to = resolve_commit(to_commit)?;

    let out = git(&["diff", "--name-only", &from, &to])?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// 3 段の線形履歴（c1 → c2 で a.txt、c2 → c3 で b.txt）。
    fn init_linear_repo() -> (TempDir, String, String, String) {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        git_in(root, &["init", "-b", "main"]);
        git_in(root, &["config", "user.email", "vrt@test.local"]);
        git_in(root, &["config", "user.name", "vrt test"]);

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

    /// ハイフン始まりの ref は `--end-of-options` 無しだと git がオプション解釈する。
    /// 修正前の `resolve_commit` はここで失敗する positive control。
    #[test]
    fn resolve_commit_accepts_hyphen_prefixed_ref() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, _c1, _c2, c3) = init_linear_repo();
        let root = tmp.path();
        git_in(root, &["update-ref", "refs/heads/-hyphen-branch", &c3]);

        let without_eoo = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                &format!("-hyphen-branch^{{commit}}"),
            ])
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
