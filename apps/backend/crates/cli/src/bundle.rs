//! `storybook-static` ディレクトリの zip 化。
//!
//! サーバー側の 200MB 上限を送る前に検査し、`index.json` が直下に無い
//! （= storybook-static ではないディレクトリを指した）ケースを分かりやすく弾く。

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use zip::write::SimpleFileOptions;

/// サーバー側 `render_service::MAX_BUNDLE_BYTES` に合わせた 200MB。
pub const MAX_BUNDLE_BYTES: usize = 200 * 1024 * 1024;

/// ディレクトリを zip 化してバイト列で返す。
///
/// - `index.json` が直下に無ければ「storybook-static を指定して」と分かるエラー
/// - 生成した zip が 200MB を超えたら送る前にエラー
pub fn zip_dir(dir: &Path) -> Result<Vec<u8>> {
    if !dir.is_dir() {
        bail!("`{}` is not a directory", dir.display());
    }
    // index.json は Storybook のビルド成果物の直下にある。無ければ指定ミス。
    if !dir.join("index.json").is_file() {
        bail!(
            "`{}` does not contain index.json at its root; \
             point --dir at the storybook-static output directory (the result of `storybook build`)",
            dir.display()
        );
    }

    let buf = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buf);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(dir, &mut entries)?;
    // zip 内のエントリ順を決定的にする。
    entries.sort();

    for path in entries {
        let rel = path
            .strip_prefix(dir)
            .expect("collected path is under dir")
            .to_string_lossy()
            // zip 内は常に `/` 区切り。
            .replace('\\', "/");
        let data =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        writer
            .start_file(rel, options)
            .context("failed to start zip entry")?;
        writer
            .write_all(&data)
            .context("failed to write zip entry")?;
    }

    let bytes = writer
        .finish()
        .context("failed to finalize zip")?
        .into_inner();

    if bytes.len() > MAX_BUNDLE_BYTES {
        bail!(
            "storybook bundle is {} bytes, over the {} byte ({} MB) server limit",
            bytes.len(),
            MAX_BUNDLE_BYTES,
            MAX_BUNDLE_BYTES / 1024 / 1024
        );
    }

    Ok(bytes)
}

/// `dir` 以下のファイルを再帰的に集める（ディレクトリは辿るだけ）。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        let entry = entry.context("failed to read directory entry")?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .context("failed to stat directory entry")?;
        if file_type.is_dir() {
            collect_files(&path, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
        // シンボリックリンクは storybook-static には通常無いので辿らない。
    }
    Ok(())
}
