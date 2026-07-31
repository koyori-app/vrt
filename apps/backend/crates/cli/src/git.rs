//! git との橋渡し。
//!
//! libgit2 系は使わず `git` バイナリを subprocess で呼ぶ（CI に git は必ずある、
//! かつ shallow clone 等の状態も git 本体の判断にそのまま乗れるため）。

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

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

/// 現在の commit SHA（`git rev-parse HEAD`）。
pub fn head_commit() -> Result<String> {
    git(&["rev-parse", "HEAD"]).context("could not resolve HEAD commit")
}

/// baseline コミットが手元に存在するか（`git cat-file -e` 相当）。
///
/// `--baseline-commit` を明示したときは存在しなければ呼び出し側で終了コード 2 にする。
pub fn commit_exists(baseline_commit: &str) -> Result<()> {
    let exists = Command::new("git")
        .args(["cat-file", "-e", &format!("{baseline_commit}^{{commit}}")])
        .output()
        .context("failed to spawn `git cat-file`")?;
    if exists.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "baseline commit {baseline_commit} is not present locally \
             (a shallow clone may be missing history; fetch with enough depth to reach it)"
        ))
    }
}

/// baseline から HEAD までの変更ファイル一覧（リポジトリルート相対）。
///
/// baseline コミットが手元に無い場合（shallow clone 等）はエラーを返し、
/// 呼び出し側で全撮影にフォールバックさせる。
pub fn changed_files(baseline_commit: &str) -> Result<Vec<String>> {
    commit_exists(baseline_commit)?;

    let out = git(&["diff", "--name-only", baseline_commit, "HEAD"])?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}
