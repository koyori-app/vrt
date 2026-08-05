//! テスト専用: host の Git 設定から独立した一時リポジトリの初期化ヘルパー。
//!
//! `cargo test` は開発者や CI の global / system gitconfig を継承した環境で走る。
//! 署名（`commit.gpgsign`）や hooks（`core.hooksPath`）が host 側で有効だと、
//! 一時リポジトリでの `git commit` が鍵や hook の有無に依存して落ちる。
//! テスト用一時リポジトリの初期化は必ず [`init_test_repo`] を通し、
//! リポジトリローカル設定（global / system を上書きする）で host 依存を遮断する。
//!
//! lib の unit test からは `crate::test_support`、bin の unit test と統合テストからは
//! self dev-dependency（feature `test-support`）経由で `vrt_cli::test_support` として使う。

use std::path::Path;
use std::process::Command;

/// `dir` を作業ディレクトリに git コマンドを実行し、失敗したら stderr 付きで panic する。
pub fn git_in(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "`git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `dir` を作業ディレクトリに git コマンドを実行し、stdout を trim して返す。
pub fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "`git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// テスト用一時リポジトリを `root` に初期化する。
///
/// host 依存の遮断（いずれもリポジトリローカル設定）:
/// - `init -b main`: host の `init.defaultBranch` に依存しない
/// - `commit.gpgsign=false` / `tag.gpgsign=false`: host で署名が有効でも
///   署名経路に入らない（鍵が無い環境で commit が失敗しない）
/// - `core.hooksPath=<存在しないパス>`: global の `core.hooksPath`（husky 等）や
///   `init.templateDir` 由来で持ち込まれた hook を実行しない
/// - `user.email` / `user.name`: host に identity 設定が無くても commit できる
///
/// 設定しないもの: `user.signingkey` / `gpg.format` は `gpgsign=false` で参照されない。
/// `core.autocrlf` 等の改行変換は、ファイル名しか比較しない現行テストに影響しないため既定のまま。
pub fn init_test_repo(root: &Path) {
    git_in(root, &["init", "-b", "main"]);
    git_in(root, &["config", "commit.gpgsign", "false"]);
    git_in(root, &["config", "tag.gpgsign", "false"]);
    let hooks_disabled = root.join("hooks-disabled");
    git_in(
        root,
        &[
            "config",
            "core.hooksPath",
            hooks_disabled.to_str().expect("utf-8 tempdir path"),
        ],
    );
    git_in(root, &["config", "user.email", "vrt@test.local"]);
    git_in(root, &["config", "user.name", "vrt test"]);
}
