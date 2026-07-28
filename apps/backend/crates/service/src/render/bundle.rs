//! Storybook バンドル（`storybook-static` の zip）の展開とストーリー一覧の抽出。
//!
//! ## 安全性
//!
//! zip は CI から送られてくる**信頼できない入力**なので、展開時に次を守る。
//!
//! - **zip-slip 対策**: エントリ名に `..` / 絶対パス / Windows のドライブ接頭辞が
//!   含まれていたら即エラー。さらに解決後のパスが展開先ディレクトリ配下にあることを
//!   正規化（canonicalize）して検証する
//! - **シンボリックリンクの拒否**: リンク先で展開先の外に出られるため、
//!   symlink エントリはエラーにする
//! - **zip bomb 対策**: エントリ数 [`MAX_ENTRIES`] と
//!   展開後の総バイト数 [`MAX_UNCOMPRESSED_BYTES`] で打ち切る

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// アップロードできる zip の上限（200MB）。
pub const MAX_BUNDLE_BYTES: usize = 200 * 1024 * 1024;
/// zip に含められるエントリ数の上限。
pub const MAX_ENTRIES: usize = 20_000;
/// 展開後の総バイト数の上限（500MB）。zip bomb で一時ディスクを食い潰させない。
pub const MAX_UNCOMPRESSED_BYTES: u64 = 500 * 1024 * 1024;

/// Storybook の index を探すときに見に行くファイル名。
/// 7 以降は `index.json`、6 系の名残として `stories.json` も見る。
const INDEX_FILE_NAMES: [&str; 2] = ["index.json", "stories.json"];

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("bundle is too large: {0} bytes (max {MAX_BUNDLE_BYTES})")]
    TooLarge(usize),
    #[error("bundle has too many entries (max {MAX_ENTRIES})")]
    TooManyEntries,
    #[error("bundle expands to more than {MAX_UNCOMPRESSED_BYTES} bytes")]
    TooMuchData,
    #[error("unsafe zip entry path: {0}")]
    UnsafePath(String),
    #[error("symlinks are not allowed in a storybook bundle: {0}")]
    Symlink(String),
    #[error("invalid zip archive: {0}")]
    InvalidZip(String),
    #[error(
        "index.json not found in the bundle (expected at the archive root or one directory below)"
    )]
    IndexNotFound,
    #[error("invalid storybook index: {0}")]
    InvalidIndex(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// バンドルから見つかった 1 ストーリー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    /// `iframe.html?id=` に渡す ID。
    pub id: String,
    /// コンポーネントのタイトル（例 `Components/Button`）。
    pub title: String,
    /// ストーリー名（例 `Primary`）。
    pub name: String,
}

impl Story {
    /// スクリーンショット名。`{title}/{name}`（例 `Components/Button/Primary`）。
    pub fn screenshot_name(&self) -> String {
        format!("{}/{}", self.title, self.name)
    }
}

/// 展開済みバンドル。`root` は静的配信のドキュメントルート（`iframe.html` がある階層）。
#[derive(Debug, Clone)]
pub struct ExtractedBundle {
    pub root: PathBuf,
    pub stories: Vec<Story>,
}

// ── 展開 ────────────────────────────────────────────────────────────────

/// 展開時に守る上限。既定値は [`ExtractLimits::default`]（= 各 `MAX_*` 定数）。
///
/// テストから小さい値を差し込めるようにするためだけに分けてある。
/// 本番経路（[`extract_zip`] / [`extract_and_index`]）は必ず既定値を使う。
#[derive(Debug, Clone, Copy)]
pub struct ExtractLimits {
    pub max_bundle_bytes: usize,
    pub max_entries: usize,
    pub max_uncompressed_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_bundle_bytes: MAX_BUNDLE_BYTES,
            max_entries: MAX_ENTRIES,
            max_uncompressed_bytes: MAX_UNCOMPRESSED_BYTES,
        }
    }
}

/// zip を `dest` に展開する。`dest` は呼び出し側が用意した空ディレクトリであること。
pub fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), BundleError> {
    extract_zip_with_limits(bytes, dest, ExtractLimits::default())
}

/// 上限を明示して展開する。[`extract_zip`] の実体。
pub fn extract_zip_with_limits(
    bytes: &[u8],
    dest: &Path,
    limits: ExtractLimits,
) -> Result<(), BundleError> {
    if bytes.len() > limits.max_bundle_bytes {
        return Err(BundleError::TooLarge(bytes.len()));
    }

    std::fs::create_dir_all(dest)?;
    // 以降の「展開先の外に出ていないか」判定は正規化済みパスで行う。
    let dest_root = dest.canonicalize()?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| BundleError::InvalidZip(e.to_string()))?;

    if archive.len() > limits.max_entries {
        return Err(BundleError::TooManyEntries);
    }

    let mut written: u64 = 0;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| BundleError::InvalidZip(e.to_string()))?;

        let raw_name = entry.name().to_string();

        // シンボリックリンクは展開先の外を指せるので受け付けない。
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(BundleError::Symlink(raw_name));
        }

        let target = safe_join(&dest_root, &raw_name)?;

        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }

        // まず zip のヘッダが申告するサイズで足切りする（実読み込み前に落とせる）。
        // ヘッダは詐称できるので、下の書き込みループで実バイト数も数える。
        if written.saturating_add(entry.size()) > limits.max_uncompressed_bytes {
            return Err(BundleError::TooMuchData);
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
            // 途中のディレクトリが（既存の）シンボリックリンクで外に出ていないか確認する。
            let parent_real = parent.canonicalize()?;
            if !parent_real.starts_with(&dest_root) {
                return Err(BundleError::UnsafePath(raw_name));
            }
        }

        let mut out = std::fs::File::create(&target)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            // 実際に書いたバイト数の累計で打ち切る。宣言サイズが嘘でもここで止まる。
            written = written.saturating_add(n as u64);
            if written > limits.max_uncompressed_bytes {
                return Err(BundleError::TooMuchData);
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
        }
    }

    Ok(())
}

/// zip エントリ名を展開先ディレクトリ配下の安全なパスへ解決する。
///
/// `..` / 絶対パス / ドライブ接頭辞を含むものは、書き込みを試みる前に弾く。
fn safe_join(dest_root: &Path, name: &str) -> Result<PathBuf, BundleError> {
    if name.is_empty() {
        return Err(BundleError::UnsafePath(name.to_string()));
    }
    // Windows 由来の `\` 区切りも `..` を隠す経路になるので、明示的に拒否する。
    if name.contains('\\') {
        return Err(BundleError::UnsafePath(name.to_string()));
    }

    let candidate = Path::new(name);
    let mut resolved = dest_root.to_path_buf();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            // `./` は無害なので読み飛ばす。
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(BundleError::UnsafePath(name.to_string()));
            }
        }
    }

    if resolved == dest_root || !resolved.starts_with(dest_root) {
        return Err(BundleError::UnsafePath(name.to_string()));
    }

    Ok(resolved)
}

// ── index.json ──────────────────────────────────────────────────────────

/// 展開済みディレクトリから index を探す。
///
/// zip の作り方が「`storybook-static/` ごと固める」か「中身だけ固める」かで
/// 1 階層ズレるため、ルート直下と 1 階層下まで見る。返すのは index があった
/// ディレクトリ（= 静的配信のドキュメントルート）と index ファイルのパス。
pub fn locate_index(root: &Path) -> Result<(PathBuf, PathBuf), BundleError> {
    for name in INDEX_FILE_NAMES {
        let candidate = root.join(name);
        if candidate.is_file() {
            return Ok((root.to_path_buf(), candidate));
        }
    }

    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(root)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();

    for dir in subdirs {
        for name in INDEX_FILE_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok((dir.clone(), candidate));
            }
        }
    }

    Err(BundleError::IndexNotFound)
}

/// Storybook 7/8/9 の `index.json`（v4/v5）。
///
/// v4 以降は `entries` がマップ。6 系の `stories.json` は `stories` なので両方見る。
#[derive(Debug, Deserialize)]
struct StorybookIndex {
    #[serde(default)]
    entries: Option<std::collections::BTreeMap<String, IndexEntry>>,
    #[serde(default)]
    stories: Option<std::collections::BTreeMap<String, IndexEntry>>,
}

#[derive(Debug, Deserialize)]
struct IndexEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// v4 以降にだけ存在する。`docs` の項目は撮らない。
    #[serde(default, rename = "type")]
    entry_type: Option<String>,
}

/// index の JSON からストーリー一覧を取り出す（`docs` などは除外）。
///
/// `type` が無い古い形式（6 系 `stories.json`）はすべてストーリーとして扱う。
pub fn parse_index(json: &str) -> Result<Vec<Story>, BundleError> {
    let index: StorybookIndex =
        serde_json::from_str(json).map_err(|e| BundleError::InvalidIndex(e.to_string()))?;

    let entries = index
        .entries
        .or(index.stories)
        .ok_or_else(|| BundleError::InvalidIndex("no `entries` or `stories` key".into()))?;

    let mut stories: Vec<Story> = entries
        .into_iter()
        .filter(|(_, entry)| entry.entry_type.as_deref().unwrap_or("story") == "story")
        .filter_map(|(key, entry)| {
            let id = entry.id.unwrap_or(key);
            Some(Story {
                title: entry.title?,
                name: entry.name?,
                id,
            })
        })
        .collect();

    // 撮影順（= スクリーンショット名の順）を決定的にする。
    stories.sort_by_key(|a| a.screenshot_name());
    Ok(stories)
}

/// zip を展開してストーリー一覧まで取り出す。
pub fn extract_and_index(bytes: &[u8], dest: &Path) -> Result<ExtractedBundle, BundleError> {
    extract_zip(bytes, dest)?;
    let (root, index_path) = locate_index(dest)?;
    let json = std::fs::read_to_string(&index_path)?;
    let stories = parse_index(&json)?;
    Ok(ExtractedBundle { root, stories })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// テスト用の zip をメモリ上で組み立てる。
    fn zip_with(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, contents) in files {
                writer.start_file(*name, options).expect("start file");
                writer.write_all(contents.as_bytes()).expect("write");
            }
            writer.finish().expect("finish zip");
        }
        buf.into_inner()
    }

    const INDEX_V5: &str = r#"{
        "v": 5,
        "entries": {
            "components-button--primary": {
                "type": "story",
                "id": "components-button--primary",
                "title": "Components/Button",
                "name": "Primary",
                "importPath": "./src/Button.stories.tsx"
            },
            "components-button--docs": {
                "type": "docs",
                "id": "components-button--docs",
                "title": "Components/Button",
                "name": "Docs",
                "importPath": "./src/Button.mdx"
            },
            "components-card--default": {
                "type": "story",
                "id": "components-card--default",
                "title": "Components/Card",
                "name": "Default",
                "importPath": "./src/Card.stories.tsx"
            }
        }
    }"#;

    #[test]
    fn extracts_bundle_and_lists_stories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip = zip_with(&[
            ("index.json", INDEX_V5),
            ("iframe.html", "<html><body>iframe</body></html>"),
            ("assets/main.js", "console.log('hi')"),
        ]);

        let bundle = extract_and_index(&zip, dir.path()).expect("extract");

        assert_eq!(bundle.root, dir.path());
        assert!(bundle.root.join("iframe.html").is_file());
        assert!(bundle.root.join("assets/main.js").is_file());

        let names: Vec<String> = bundle.stories.iter().map(|s| s.screenshot_name()).collect();
        assert_eq!(
            names,
            vec!["Components/Button/Primary", "Components/Card/Default"],
            "docs entries must be skipped and stories sorted by name"
        );
        assert_eq!(bundle.stories[0].id, "components-button--primary");
    }

    #[test]
    fn finds_index_one_directory_below_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip = zip_with(&[
            ("storybook-static/index.json", INDEX_V5),
            ("storybook-static/iframe.html", "<html></html>"),
        ]);

        let bundle = extract_and_index(&zip, dir.path()).expect("extract");
        assert_eq!(bundle.root, dir.path().join("storybook-static"));
        assert_eq!(bundle.stories.len(), 2);
    }

    #[test]
    fn rejects_zip_slip_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip = zip_with(&[("../escaped.txt", "pwned"), ("index.json", INDEX_V5)]);

        let err = extract_zip(&zip, dir.path()).expect_err("zip slip must be rejected");
        assert!(
            matches!(err, BundleError::UnsafePath(_)),
            "expected UnsafePath, got {err:?}"
        );
        // 展開先の外に何も書かれていないこと。
        assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
    }

    #[test]
    fn rejects_absolute_and_backslash_entries() {
        let dest = tempfile::tempdir().expect("tempdir");
        let root = dest.path().canonicalize().expect("canonicalize");
        assert!(matches!(
            safe_join(&root, "/etc/passwd"),
            Err(BundleError::UnsafePath(_))
        ));
        assert!(matches!(
            safe_join(&root, "..\\..\\windows"),
            Err(BundleError::UnsafePath(_))
        ));
        assert!(matches!(
            safe_join(&root, "a/../../b"),
            Err(BundleError::UnsafePath(_))
        ));
        // 素直なパスと `./` 付きは通る。
        assert_eq!(
            safe_join(&root, "./iframe.html").unwrap(),
            root.join("iframe.html")
        );
        assert_eq!(
            safe_join(&root, "assets/app.js").unwrap(),
            root.join("assets").join("app.js")
        );
    }

    #[test]
    fn rejects_symlink_entries() {
        let dir = tempfile::tempdir().expect("tempdir");

        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            // 展開先の外を指すシンボリックリンク。
            writer
                .add_symlink("evil-link", "/etc/passwd", options)
                .expect("add symlink");
            writer.finish().expect("finish zip");
        }

        let err = extract_zip(&buf.into_inner(), dir.path()).expect_err("symlink must be rejected");
        assert!(matches!(err, BundleError::Symlink(_)), "got {err:?}");
        assert!(!dir.path().join("evil-link").exists());
    }

    #[test]
    fn rejects_bundles_with_too_many_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip = zip_with(&[("a.txt", "a"), ("b.txt", "b"), ("c.txt", "c")]);

        let limits = ExtractLimits {
            max_entries: 2,
            ..ExtractLimits::default()
        };
        let err = extract_zip_with_limits(&zip, dir.path(), limits).expect_err("entry cap");
        assert!(matches!(err, BundleError::TooManyEntries), "got {err:?}");
        // エントリ数の判定は 1 バイトも書く前に済ませる。
        assert!(!dir.path().join("a.txt").exists());
    }

    #[test]
    fn rejects_bundles_that_expand_past_the_uncompressed_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        // よく圧縮される（= 圧縮後は小さいのに展開すると大きい）中身にする。
        let payload = "0".repeat(64 * 1024);
        let zip = zip_with(&[("index.json", INDEX_V5), ("big.js", payload.as_str())]);

        let limits = ExtractLimits {
            max_uncompressed_bytes: 1024,
            ..ExtractLimits::default()
        };
        let err = extract_zip_with_limits(&zip, dir.path(), limits).expect_err("size cap");
        assert!(matches!(err, BundleError::TooMuchData), "got {err:?}");
    }

    #[test]
    fn rejects_bundles_larger_than_the_upload_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip = zip_with(&[("index.json", INDEX_V5)]);

        let limits = ExtractLimits {
            max_bundle_bytes: 8,
            ..ExtractLimits::default()
        };
        assert!(matches!(
            extract_zip_with_limits(&zip, dir.path(), limits),
            Err(BundleError::TooLarge(_))
        ));
    }

    #[test]
    fn missing_index_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip = zip_with(&[("iframe.html", "<html></html>")]);
        let err = extract_and_index(&zip, dir.path()).expect_err("missing index");
        assert!(matches!(err, BundleError::IndexNotFound), "got {err:?}");
    }

    #[test]
    fn docs_only_index_yields_no_stories() {
        let stories = parse_index(
            r#"{"v":5,"entries":{"a--docs":{"type":"docs","title":"A","name":"Docs"}}}"#,
        )
        .expect("parse");
        assert!(stories.is_empty());
    }

    #[test]
    fn supports_v6_stories_key_without_type() {
        let stories = parse_index(
            r#"{"v":3,"stories":{"a--b":{"id":"a--b","title":"A","name":"B","importPath":"./a.js"}}}"#,
        )
        .expect("parse");
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].screenshot_name(), "A/B");
    }

    #[test]
    fn malformed_index_is_an_error() {
        assert!(matches!(
            parse_index("not json"),
            Err(BundleError::InvalidIndex(_))
        ));
        assert!(matches!(
            parse_index(r#"{"v":5}"#),
            Err(BundleError::InvalidIndex(_))
        ));
    }

    #[test]
    fn screenshot_name_joins_title_and_name() {
        let story = Story {
            id: "components-button--primary".into(),
            title: "Components/Button".into(),
            name: "Primary".into(),
        };
        assert_eq!(story.screenshot_name(), "Components/Button/Primary");
    }
}
