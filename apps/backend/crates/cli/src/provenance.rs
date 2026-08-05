//! 成果物（storybook-static）を生成元コミットへ束縛する provenance。
//!
//! 絞り込み（`vrt plan` / `vrt upload --only-changed`）の実入力は worktree の
//! `preview-stats.json` と `index.json` である。両者は通常 **untracked** なので、
//! `git status` ベースの worktree 検査（HEAD 一致・追跡ファイル clean）では
//! 「別コミットでビルドされた成果物」を検出できない——古い CI キャッシュや
//! rebase 前のビルドを掴んだまま絞り込むと、変更の影響を受けた story が
//! 選別から漏れて偽 PASS になる。
//!
//! そこで生成側は `vrt stamp -- <build command>` で **build 自体を vrt が実行し**、
//! 開始前と成功後の HEAD（同一・clean）を自分で観測してから
//! `<dir>/vrt-provenance.json` を書く。読取側（plan / upload）は絞り込みの前に
//! これを検証する。束縛は二重で行う。
//!
//! - `head_commit_sha`: build の前後で vrt 自身が観測した worktree の HEAD。
//!   plan の終点 commit と一致しなければ別コミットの成果物である
//! - `stats_sha256` / `index_sha256`: 実際に読む 2 ファイルの内容ハッシュ。
//!   stamp 後に stats / index だけ差し替えられた（キャッシュ復元等）ケースを
//!   コミット照合だけでは検出できないため、実入力のバイト列そのものを束縛する
//!
//! version 1（build 所有なしの旧形式）は「stamp 時点の HEAD」しか証明せず、
//! build と stamp の間で checkout が挟まると嘘の証明になる。そのため v1 は
//! 拒否も採用もせず**全撮影へ倒す**（[`Verification::Unowned`]）。
//!
//! 倒し方（fail-closed）:
//!
//! - provenance が**無い** → 全撮影へ倒す（絞り込みは行わない）。既存の
//!   「stats が無ければ全撮影」と同じ扱いで、stamp 未導入のパイプラインを
//!   壊さない移行経路になる。全撮影は撮り逃しを作らないので偽 PASS はない
//! - provenance が **version 1（build 所有なし）** → 全撮影へ倒す。生成時点を
//!   観測していない証明を信じて絞り込まない（移行期: 旧 CLI で stamp された
//!   キャッシュはこの経路で無害化される）
//! - provenance が**あるのに壊れている・コミットが合わない・ハッシュが合わない**
//!   → エラー（終了コード 2）。設定ミスの積極的な証拠なので、黙って全撮影に
//!   読み替えず気づかせる（worktree 不一致をエラーにするのと同じ方針）
//!
//! いずれの場合も「検証できないまま絞り込む」ことはない。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// provenance ファイル名。成果物ディレクトリ直下に置く。
pub const PROVENANCE_FILE: &str = "vrt-provenance.json";

/// provenance の契約 version。互換を壊す変更を入れるときだけ上げる。
///
/// v2 = vrt が build を所有した証明（`build_command` 必須）。
/// v1 = stamp 単独の旧形式。生成時点を観測していないため絞り込みには使わない
/// （[`verify`] が [`Verification::Unowned`] を返し、呼び出し側は全撮影へ倒す）。
pub const PROVENANCE_VERSION: u32 = 2;

/// v1（build 所有なしの旧形式）。読み取り時の判別にのみ使う。
const LEGACY_UNOWNED_VERSION: u32 = 1;

/// `vrt stamp -- <build command>` が書き、plan / upload が検証する内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub version: u32,
    /// build の前後で vrt が観測した worktree の HEAD commit OID。
    pub head_commit_sha: String,
    /// `preview-stats.json`（解決後のパス）の SHA-256（hex）。
    pub stats_sha256: String,
    /// `index.json`（解決後のパス）の SHA-256（hex）。
    pub index_sha256: String,
    /// vrt が実行した build コマンド（argv）。build 所有の証明であり空は不正。
    pub build_command: Vec<String>,
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

/// 検証結果。エラーにならなかった 3 状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// provenance が存在し、build 所有・コミット・内容ハッシュとも一致した。
    /// 絞り込んでよい。照合に使った stats / index のバイト列そのものを持ち回る。
    Verified(VerifiedArtifacts),
    /// provenance が存在しない。絞り込まず全撮影へ倒すこと（移行期の既定）。
    Missing,
    /// v1（build 所有なしの旧形式）。生成時点を観測していない証明なので
    /// 採用せず、絞り込まず全撮影へ倒すこと。
    Unowned,
}

/// [`verify`] がハッシュ照合に使った stats / index の内容そのもの。
///
/// 選別（[`crate::plan::select_stories_from_verified`]）は必ずこの値から
/// 読む——verify の後にファイルを読み直すと、照合したバイト列と選別に使う
/// バイト列の同一性がプロセス内で保証されず、その間の差し替え（TOCTOU）を
/// 内容ハッシュの束縛がすり抜ける。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifacts {
    /// `preview-stats.json`（解決後のパス）の内容。
    pub stats_raw: String,
    /// `index.json`（解決後のパス）の内容。
    pub index_raw: String,
}

fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(out, "{byte:02x}").expect("write to String never fails");
    }
    out
}

fn sha256_hex(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(sha256_hex_bytes(&bytes))
}

/// build 開始前に旧 provenance と絞り込み入力（stats / index）を削除する。
///
/// 「build コマンドが成功した」ことと「成果物がその build で生成された」ことは
/// 別である——何も生成しない命令（no-op）でも成功はするので、build 前に残って
/// いた別コミットの成果物がそのまま stamp されてしまう。そこで build 前に
/// 実入力の 2 ファイルと旧 provenance を消す。削除後に [`stamp`] が stats /
/// index の存在を要求するため、build 成功後にそれらが存在すること自体が
/// 「build の実行中に生成された」証明になる。
///
/// 副次効果として、build が失敗した stamp も旧 provenance を残さない。
/// 失敗した stamp の後に古い証明が生き残って再利用される経路はここで断たれる。
///
/// stats / index が git 追跡下にある場合、この削除は worktree を汚し
/// build 後の clean 再検査で stamp が拒否される。build 出力を追跡する構成は
/// そもそも「build がその commit で走った」証明と両立しないため、fail-closed
/// のままにしてある。
pub fn invalidate(paths: &ArtifactPaths<'_>) -> Result<()> {
    for path in [
        paths.provenance_path(),
        paths.stats_path(),
        paths.index_path(),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to remove stale {}", path.display()));
            }
        }
    }
    Ok(())
}

/// provenance を書き込む（`vrt stamp` の本体）。
///
/// stats / index の両方が build 後に存在しなければエラー。stamp の目的は
/// 絞り込みを有効にすることであり、stats 無しでは絞り込み自体が成立しないため
/// （build コマンドに `--stats-json` を足させる）。
///
/// git の解決（HEAD の取得・worktree clean 検査）と build コマンドの実行は
/// 呼び出し側の責務。ここを純関数（fs のみ）に保つことでテストが git に
/// 依存しない。`build_command` は build 所有の証明なので空を拒否する。
pub fn stamp(
    paths: &ArtifactPaths<'_>,
    head_commit_sha: &str,
    build_command: &[String],
) -> Result<PathBuf> {
    if build_command.is_empty() {
        bail!("a provenance without the build command would not prove build ownership");
    }
    let stats_path = paths.stats_path();
    if !stats_path.is_file() {
        bail!(
            "stats file {} not found; the build command must run `storybook build --stats-json`",
            stats_path.display()
        );
    }
    let index_path = paths.index_path();
    if !index_path.is_file() {
        bail!(
            "index file {} not found; the build command must run `storybook build`",
            index_path.display()
        );
    }

    let provenance = Provenance {
        version: PROVENANCE_VERSION,
        head_commit_sha: head_commit_sha.to_string(),
        stats_sha256: sha256_hex(&stats_path)?,
        index_sha256: sha256_hex(&index_path)?,
        build_command: build_command.to_vec(),
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
/// - version 1（build 所有なしの旧形式）→ `Ok(Unowned)`（呼び出し側は全撮影へ
///   倒す。生成時点を観測していない証明なので、正しく見えても採用しない）
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
    // v1 には build_command が無く Provenance として直接 parse できないため、
    // 先に version だけを見る。
    let value: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse {}; re-run `vrt stamp -- <build command>`",
            path.display()
        )
    })?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .with_context(|| format!("{} has no numeric `version` field", path.display()))?;
    if version == u64::from(LEGACY_UNOWNED_VERSION) {
        return Ok(Verification::Unowned);
    }
    if version != u64::from(PROVENANCE_VERSION) {
        bail!(
            "unsupported provenance version {} in {} (this CLI understands {}); \
             use a matching `vrt` for stamp and plan",
            version,
            path.display(),
            PROVENANCE_VERSION
        );
    }
    let provenance: Provenance = serde_json::from_value(value).with_context(|| {
        format!(
            "failed to parse {}; re-run `vrt stamp -- <build command>`",
            path.display()
        )
    })?;
    if provenance.build_command.is_empty() {
        bail!(
            "{} records no build command, so it does not prove the artifact was built \
             by `vrt stamp -- <build command>`; re-stamp with the build command",
            path.display()
        );
    }
    if provenance.head_commit_sha != expected_head_commit_sha {
        bail!(
            "the artifact in {} was built from commit {} but the plan targets {}; \
             re-run `vrt stamp -- <build command>` at the target commit \
             instead of reusing a stale artifact",
            paths.dir.display(),
            provenance.head_commit_sha,
            expected_head_commit_sha
        );
    }

    // 実入力は 1 度だけ読み、ハッシュ照合したバイト列そのものを返す。
    // 照合と選別で別々に読むと、その間の差し替えが束縛をすり抜ける（TOCTOU）。
    let stats_path = paths.stats_path();
    let stats_raw = std::fs::read_to_string(&stats_path).with_context(|| {
        format!(
            "could not read {} to verify provenance",
            stats_path.display()
        )
    })?;
    if sha256_hex_bytes(stats_raw.as_bytes()) != provenance.stats_sha256 {
        bail!(
            "{} does not match the stamped artifact (its content changed after `vrt stamp`); \
             re-run `vrt stamp -- <build command>` so the stamp observes the build",
            stats_path.display()
        );
    }

    let index_path = paths.index_path();
    let index_raw = std::fs::read_to_string(&index_path).with_context(|| {
        format!(
            "could not read {} to verify provenance",
            index_path.display()
        )
    })?;
    if sha256_hex_bytes(index_raw.as_bytes()) != provenance.index_sha256 {
        bail!(
            "{} does not match the stamped artifact (its content changed after `vrt stamp`); \
             re-run `vrt stamp -- <build command>` so the stamp observes the build",
            index_path.display()
        );
    }

    Ok(Verification::Verified(VerifiedArtifacts {
        stats_raw,
        index_raw,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn build_command() -> Vec<String> {
        vec!["storybook".to_string(), "build".to_string()]
    }

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
        let out = stamp(&paths(tmp.path()), HEAD, &build_command()).expect("stamp");
        assert_eq!(out, tmp.path().join(PROVENANCE_FILE));
        // Verified は照合したバイト列そのものを返す（選別はこれを読む——read-once）。
        assert_eq!(
            verify(&paths(tmp.path()), HEAD).expect("verify"),
            Verification::Verified(VerifiedArtifacts {
                stats_raw: r#"{"modules":[]}"#.to_string(),
                index_raw: r#"{"v":5,"entries":{}}"#.to_string(),
            })
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
        stamp(&paths(tmp.path()), OTHER, &build_command()).expect("stamp");
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
        stamp(&paths(tmp.path()), HEAD, &build_command()).expect("stamp");
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
        stamp(&paths(tmp.path()), HEAD, &build_command()).expect("stamp");
        fs::write(
            tmp.path().join("index.json"),
            r#"{"v":5,"entries":{"x":{}}}"#,
        )
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
        stamp(&paths(tmp.path()), HEAD, &build_command()).expect("stamp");
        let path = tmp.path().join(PROVENANCE_FILE);
        let raw = fs::read_to_string(&path).expect("read");
        fs::write(&path, raw.replace("\"version\": 2", "\"version\": 999")).expect("bump");
        let err = verify(&paths(tmp.path()), HEAD).expect_err("must reject");
        assert!(format!("{err:#}").contains("version 999"), "err={err:#}");
    }

    /// v1（build 所有なしの旧形式）は、コミットもハッシュも正しく見えても
    /// 採用しない——生成時点を観測していない証明だからである。エラーでもなく、
    /// 全撮影へ倒す Unowned として報告する（移行期のキャッシュ無害化経路）。
    /// positive control: v1 を検証して通していた修正前の実装ではここが
    /// Verified になり、このテストは落ちる。
    #[test]
    fn legacy_v1_provenance_is_unowned_not_verified() {
        let tmp = artifact_dir();
        let ap = paths(tmp.path());
        let v1 = serde_json::json!({
            "version": 1,
            "head_commit_sha": HEAD,
            "stats_sha256": sha256_hex(&ap.stats_path()).expect("hash stats"),
            "index_sha256": sha256_hex(&ap.index_path()).expect("hash index"),
        });
        fs::write(
            tmp.path().join(PROVENANCE_FILE),
            serde_json::to_string_pretty(&v1).expect("serialize"),
        )
        .expect("write v1");
        assert_eq!(
            verify(&paths(tmp.path()), HEAD).expect("verify"),
            Verification::Unowned
        );
    }

    /// v2 なのに build_command が空の provenance は build 所有の証明にならない。
    /// Unowned へ倒さずエラーにする（v2 を書けるのは所有経路だけのはずで、
    /// 空は改変か生成バグの積極的な証拠だから）。
    #[test]
    fn v2_without_build_command_is_rejected() {
        let tmp = artifact_dir();
        stamp(&paths(tmp.path()), HEAD, &build_command()).expect("stamp");
        let path = tmp.path().join(PROVENANCE_FILE);
        let raw = fs::read_to_string(&path).expect("read");
        let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        value["build_command"] = serde_json::json!([]);
        fs::write(&path, value.to_string()).expect("strip command");
        let err = verify(&paths(tmp.path()), HEAD).expect_err("must reject");
        assert!(
            format!("{err:#}").contains("records no build command"),
            "err={err:#}"
        );
    }

    /// stamp 自体も空の build コマンドを拒否する（所有なしの v2 を作らせない）。
    #[test]
    fn stamp_rejects_an_empty_build_command() {
        let tmp = artifact_dir();
        let err = stamp(&paths(tmp.path()), HEAD, &[]).expect_err("must reject");
        assert!(
            format!("{err:#}").contains("build ownership"),
            "err={err:#}"
        );
    }

    /// invalidate は provenance / stats / index の 3 点をすべて消す。
    /// 消し残しがあると no-op build が旧成果物を stamp できてしまう。
    #[test]
    fn invalidate_removes_provenance_stats_and_index() {
        let tmp = artifact_dir();
        stamp(&paths(tmp.path()), HEAD, &build_command()).expect("stamp");
        invalidate(&paths(tmp.path())).expect("invalidate");
        assert!(!tmp.path().join(PROVENANCE_FILE).is_file());
        assert!(!tmp.path().join("preview-stats.json").is_file());
        assert!(!tmp.path().join("index.json").is_file());
        // 無効化後の成果物では stamp できない（build による再生成が必須）。
        let err = stamp(&paths(tmp.path()), HEAD, &build_command()).expect_err("no stats");
        assert!(format!("{err:#}").contains("--stats-json"), "err={err:#}");
    }

    /// 消す対象が最初から無くても invalidate は成功する（初回 build の経路）。
    #[test]
    fn invalidate_tolerates_absent_files() {
        let tmp = TempDir::new().expect("tempdir");
        invalidate(&paths(tmp.path())).expect("invalidate on empty dir");
    }

    #[test]
    fn stamp_requires_stats_and_index() {
        let tmp = TempDir::new().expect("tempdir");
        let err = stamp(&paths(tmp.path()), HEAD, &build_command()).expect_err("no stats");
        assert!(format!("{err:#}").contains("--stats-json"), "err={err:#}");

        fs::write(tmp.path().join("preview-stats.json"), "{}").expect("stats");
        let err = stamp(&paths(tmp.path()), HEAD, &build_command()).expect_err("no index");
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
        stamp(&custom, HEAD, &build_command()).expect("stamp");
        assert!(matches!(
            verify(&custom, HEAD).expect("verify"),
            Verification::Verified(_)
        ));

        fs::write(&stats, r#"{"modules":[1]}"#).expect("swap");
        assert!(verify(&custom, HEAD).is_err());
    }
}
