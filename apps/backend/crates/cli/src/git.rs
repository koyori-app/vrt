//! git との橋渡し。
//!
//! libgit2 系は使わず `git` バイナリを subprocess で呼ぶ（CI に git は必ずある、
//! かつ shallow clone 等の状態も git 本体の判断にそのまま乗れるため）。

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// `git <args>` を実行し、成功なら stdout を trim して返す。
fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/git")
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
    git(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
        .with_context(|| format!("could not resolve commit {rev}"))
}

/// 現在の commit SHA（`git rev-parse HEAD`）。
pub fn head_commit() -> Result<String> {
    resolve_commit("HEAD")
}

/// baseline コミットが手元に存在するか。
///
/// [`resolve_commit`] への薄いラッパー。cmd_591 の cat-file 経路は廃止し、
/// 存在確認と OID 正規化を一本化する。
pub fn commit_exists(baseline_commit: &str) -> Result<()> {
    resolve_commit(baseline_commit).map(|_| ())
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

    #[test]
    fn resolve_commit_normalizes_head_ref() {
        let oid = resolve_commit("HEAD").expect("HEAD");
        assert_eq!(oid.len(), 40);
        assert!(oid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_commit_accepts_head_tilde_notation() {
        let head = resolve_commit("HEAD").expect("HEAD");
        // shallow clone では親が無いことがある。
        let Ok(parent) = resolve_commit("HEAD~1") else {
            return;
        };
        assert_ne!(head, parent);
    }

    #[test]
    fn changed_files_diffs_between_two_explicit_commits() {
        let Ok(parent) = resolve_commit("HEAD~1") else {
            return;
        };
        let head = resolve_commit("HEAD").expect("head");
        let files = changed_files(&parent, &head).expect("diff");
        // 差分の有無は履歴依存なので、呼び出しが成功することだけ固定する。
        let _ = files;
    }
}
