//! 成果物（storybook-static）を生成元コミットへ束縛する provenance。
//!
//! 絞り込み（`vrt plan` / `vrt upload --only-changed`）の実入力は worktree の
//! `preview-stats.json` と `index.json` である。両者は通常 **untracked** なので、
//! `git status` ベースの worktree 検査（HEAD 一致・追跡ファイル clean）では
//! 「別コミットでビルドされた成果物」を検出できない——古い CI キャッシュや
//! rebase 前のビルドを掴んだまま絞り込むと、変更の影響を受けた story が
//! 選別から漏れて偽 PASS になる。
//!
//! そこで生成側は storybook build の直後に `vrt stamp` で
//! `<dir>/vrt-provenance.json` を書き、読取側（plan / upload）は絞り込みの前に
//! これを検証する。束縛は二重で行う。
//!
//! - `head_commit_sha`: 成果物を生成した worktree の HEAD。plan の終点 commit と
//!   一致しなければ別コミットの成果物である
//! - `stats_sha256` / `index_sha256`: 実際に読む 2 ファイルの内容ハッシュ。
//!   stamp 後に stats / index だけ差し替えられた（キャッシュ復元等）ケースを
//!   コミット照合だけでは検出できないため、実入力のバイト列そのものを束縛する
//!
//! 倒し方（fail-closed）:
//!
//! - provenance が**無い** → 全撮影へ倒す（絞り込みは行わない）。既存の
//!   「stats が無ければ全撮影」と同じ扱いで、stamp 未導入のパイプラインを
//!   壊さない移行経路になる。全撮影は撮り逃しを作らないので偽 PASS はない
//! - provenance が**あるのに壊れている・コミットが合わない・ハッシュが合わない**
//!   → エラー（終了コード 2）。設定ミスの積極的な証拠なので、黙って全撮影に
//!   読み替えず気づかせる（worktree 不一致をエラーにするのと同じ方針）
//!
//! どちらの場合も「検証できないまま絞り込む」ことはない。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// provenance ファイル名。成果物ディレクトリ直下に置く。
pub const PROVENANCE_FILE: &str = "vrt-provenance.json";

/// provenance の契約 version。互換を壊す変更を入れるときだけ上げる。
pub const PROVENANCE_VERSION: u32 = 1;

/// `vrt stamp` が書き、plan / upload が検証する内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub version: u32,
    /// 成果物を生成した worktree の HEAD commit OID。
    pub head_commit_sha: String,
    /// `preview-stats.json`（解決後のパス）の SHA-256（hex）。
    pub stats_sha256: String,
    /// `index.json`（解決後のパス）の SHA-256（hex）。
    pub index_sha256: String,
}

/// 絞り込み入力の在り処。stats / index の解決規則は
/// [`crate::plan::SelectionInputs`] と同一（省略時は `dir` 直下の既定名）。
pub struct ArtifactPaths<'a> {
    pub dir: &'a Path,
    pub stats_json: Option<&'a Path>,
    pub index_json: Option<&'a Path>,
}

impl ArtifactPaths<'_> {
    pub fn stats_path(&self) -> PathBuf {
        self.stats_json
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.dir.join("preview-stats.json"))
    }

    pub fn index_path(&self) -> PathBuf {
        self.index_json
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.dir.join("index.json"))
    }

    pub fn provenance_path(&self) -> PathBuf {
        self.dir.join(PROVENANCE_FILE)
    }
}

/// 検証結果。エラーにならなかった 2 状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// provenance が存在し、コミット・内容ハッシュとも一致した。絞り込んでよい。
    Verified,
    /// provenance が存在しない。絞り込まず全撮影へ倒すこと（移行期の既定）。
    Missing,
}

fn sha256_hex(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(out, "{byte:02x}").expect("write to String never fails");
    }
    Ok(out)
}

/// provenance を書き込む（`vrt stamp` の本体）。
///
/// stats / index の両方が存在しなければエラー。stamp の目的は絞り込みを
/// 有効にすることであり、stats 無しでは絞り込み自体が成立しないため
/// （`storybook build --stats-json` を先に直させる）。
///
/// git の解決（HEAD の取得・worktree clean 検査）は呼び出し側の責務。
/// ここを純関数（fs のみ）に保つことでテストが git に依存しない。
pub fn stamp(paths: &ArtifactPaths<'_>, head_commit_sha: &str) -> Result<PathBuf> {
    let stats_path = paths.stats_path();
    if !stats_path.is_file() {
        bail!(
            "stats file {} not found; run `storybook build --stats-json` before `vrt stamp`",
            stats_path.display()
        );
    }
    let index_path = paths.index_path();
    if !index_path.is_file() {
        bail!(
            "index file {} not found; run `storybook build` before `vrt stamp`",
            index_path.display()
        );
    }

    let provenance = Provenance {
        version: PROVENANCE_VERSION,
        head_commit_sha: head_commit_sha.to_string(),
        stats_sha256: sha256_hex(&stats_path)?,
        index_sha256: sha256_hex(&index_path)?,
    };

    let out_path = paths.provenance_path();
    let json = serde_json::to_string_pretty(&provenance).context("serialize provenance")?;
    std::fs::write(&out_path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", out_path.display()))?;
    Ok(out_path)
}

/// 絞り込みの前に成果物の生成元を検証する。
///
/// - ファイルが無い → `Ok(Missing)`（呼び出し側は全撮影へ倒す）
/// - 読めない・壊れている・version 不明 → `Err`
/// - `head_commit_sha` が計画の終点 `expected_head_commit_sha` と不一致 → `Err`
/// - stats / index の内容ハッシュが記録と不一致 → `Err`
pub fn verify(paths: &ArtifactPaths<'_>, expected_head_commit_sha: &str) -> Result<Verification> {
    let path = paths.provenance_path();
    if !path.is_file() {
        return Ok(Verification::Missing);
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let provenance: Provenance = serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse {}; re-run `vrt stamp` right after `storybook build`",
            path.display()
        )
    })?;

    if provenance.version != PROVENANCE_VERSION {
        bail!(
            "unsupported provenance version {} in {} (this CLI understands {}); \
             use a matching `vrt` for stamp and plan",
            provenance.version,
            path.display(),
            PROVENANCE_VERSION
        );
    }
    if provenance.head_commit_sha != expected_head_commit_sha {
        bail!(
            "the artifact in {} was built from commit {} but the plan targets {}; \
             rebuild the storybook (and re-run `vrt stamp`) at the target commit \
             instead of reusing a stale artifact",
            paths.dir.display(),
            provenance.head_commit_sha,
            expected_head_commit_sha
        );
    }

    let stats_path = paths.stats_path();
    let actual_stats = sha256_hex(&stats_path)
        .with_context(|| format!("could not hash {} to verify provenance", stats_path.display()))?;
    if actual_stats != provenance.stats_sha256 {
        bail!(
            "{} does not match the stamped artifact (its content changed after `vrt stamp`); \
             re-run `storybook build --stats-json` and `vrt stamp` together",
            stats_path.display()
        );
    }

    let index_path = paths.index_path();
    let actual_index = sha256_hex(&index_path)
        .with_context(|| format!("could not hash {} to verify provenance", index_path.display()))?;
    if actual_index != provenance.index_sha256 {
        bail!(
            "{} does not match the stamped artifact (its content changed after `vrt stamp`); \
             re-run `storybook build` and `vrt stamp` together",
            index_path.display()
        );
    }

    Ok(Verification::Verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn artifact_dir() -> TempDir {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join("preview-stats.json"), r#"{"modules":[]}"#).expect("stats");
        fs::write(tmp.path().join("index.json"), r#"{"v":5,"entries":{}}"#).expect("index");
        tmp
    }

    fn paths(dir: &Path) -> ArtifactPaths<'_> {
        ArtifactPaths {
            dir,
            stats_json: None,
            index_json: None,
        }
    }

    #[test]
    fn stamp_then_verify_round_trips() {
        let tmp = artifact_dir();
        let out = stamp(&paths(tmp.path()), HEAD).expect("stamp");
        assert_eq!(out, tmp.path().join(PROVENANCE_FILE));
        assert_eq!(
            verify(&paths(tmp.path()), HEAD).expect("verify"),
            Verification::Verified
        );
    }

    #[test]
    fn missing_provenance_is_reported_not_an_error() {
        let tmp = artifact_dir();
        assert_eq!(
            verify(&paths(tmp.path()), HEAD).expect("verify"),
            Verification::Missing
        );
    }

    /// 別コミットで生成された成果物は拒否される。
    /// positive control: provenance を見ない修正前の実装ではこの成果物のまま
    /// 絞り込みが通っていた（本テストの検査対象そのもの）。
    #[test]
    fn artifact_from_another_commit_is_rejected() {
        let tmp = artifact_dir();
        stamp(&paths(tmp.path()), OTHER).expect("stamp");
        let err = verify(&paths(tmp.path()), HEAD).expect_err("must reject");
        let msg = format!("{err:#}");
        assert!(msg.contains(OTHER) && msg.contains(HEAD), "err={msg}");
        assert!(msg.contains("built from commit"), "err={msg}");
    }

    /// stamp 後に stats だけ差し替えられた（キャッシュ復元など）ケースは、
    /// コミットが一致していても内容ハッシュで検出する。
    #[test]
    fn stats_swapped_after_stamp_is_rejected() {
        let tmp = artifact_dir();
        stamp(&paths(tmp.path()), HEAD).expect("stamp");
        fs::write(
            tmp.path().join("preview-stats.json"),
            r#"{"modules":[{"id":1}]}"#,
        )
        .expect("swap stats");
        let err = verify(&paths(tmp.path()), HEAD).expect_err("must reject");
        assert!(
            format!("{err:#}").contains("preview-stats.json"),
            "err={err:#}"
        );
    }

    #[test]
    fn index_swapped_after_stamp_is_rejected() {
        let tmp = artifact_dir();
        stamp(&paths(tmp.path()), HEAD).expect("stamp");
        fs::write(tmp.path().join("index.json"), r#"{"v":5,"entries":{"x":{}}}"#)
            .expect("swap index");
        let err = verify(&paths(tmp.path()), HEAD).expect_err("must reject");
        assert!(format!("{err:#}").contains("index.json"), "err={err:#}");
    }

    #[test]
    fn corrupt_provenance_is_an_error_not_capture_all() {
        let tmp = artifact_dir();
        fs::write(tmp.path().join(PROVENANCE_FILE), "not json").expect("corrupt");
        let err = verify(&paths(tmp.path()), HEAD).expect_err("must reject");
        assert!(format!("{err:#}").contains("vrt stamp"), "err={err:#}");
    }

    #[test]
    fn unknown_provenance_version_is_rejected() {
        let tmp = artifact_dir();
        stamp(&paths(tmp.path()), HEAD).expect("stamp");
        let path = tmp.path().join(PROVENANCE_FILE);
        let raw = fs::read_to_string(&path).expect("read");
        fs::write(&path, raw.replace("\"version\": 1", "\"version\": 999")).expect("bump");
        let err = verify(&paths(tmp.path()), HEAD).expect_err("must reject");
        assert!(format!("{err:#}").contains("version 999"), "err={err:#}");
    }

    #[test]
    fn stamp_requires_stats_and_index() {
        let tmp = TempDir::new().expect("tempdir");
        let err = stamp(&paths(tmp.path()), HEAD).expect_err("no stats");
        assert!(format!("{err:#}").contains("--stats-json"), "err={err:#}");

        fs::write(tmp.path().join("preview-stats.json"), "{}").expect("stats");
        let err = stamp(&paths(tmp.path()), HEAD).expect_err("no index");
        assert!(format!("{err:#}").contains("index.json"), "err={err:#}");
    }

    /// カスタムパス指定（--stats-json / --index-json）でも同じ解決規則で束縛される。
    #[test]
    fn custom_paths_are_bound_too() {
        let tmp = TempDir::new().expect("tempdir");
        let stats = tmp.path().join("custom-stats.json");
        let index = tmp.path().join("custom-index.json");
        fs::write(&stats, r#"{"modules":[]}"#).expect("stats");
        fs::write(&index, r#"{"v":5,"entries":{}}"#).expect("index");
        let custom = ArtifactPaths {
            dir: tmp.path(),
            stats_json: Some(&stats),
            index_json: Some(&index),
        };
        stamp(&custom, HEAD).expect("stamp");
        assert_eq!(verify(&custom, HEAD).expect("verify"), Verification::Verified);

        fs::write(&stats, r#"{"modules":[1]}"#).expect("swap");
        assert!(verify(&custom, HEAD).is_err());
    }
}
