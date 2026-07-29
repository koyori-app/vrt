//! アップロード済み Storybook バンドルのローカル展開キャッシュと静的配信。
//!
//! 「Open Storybook」（Chromatic の View Storybook 相当）は、撮ったスクリーンショット
//! ではなく**アップロードされた Storybook そのもの**をブラウザで対話的に開かせる。
//! そのためにビルドごとの zip を初回リクエスト時に 1 度だけローカルへ展開し、
//! 以降はそのディレクトリからファイルを配信する。
//!
//! ## 安全性
//!
//! - 展開は [`super::bundle::extract_and_index`] をそのまま使う。zip-slip・シンボリック
//!   リンク・サイズ/エントリ数上限（zip bomb 対策）は展開側で担保される。
//! - 配信時のパス解決は正規化（canonicalize）+ 接頭辞チェックでキャッシュディレクトリ外へ
//!   出られないようにする（`..`・絶対パス・シンボリックリンクを拒否）。
//!
//! ## 並行性
//!
//! 同じビルドへの初回リクエストが同時に来ても二重展開・レースが起きないよう、
//! ビルドごとの非同期ロックで直列化する。展開は一時ディレクトリに対して行い、
//! 完成後にアトミックな `rename` で最終パスへ移す。したがって最終ディレクトリの存在
//! そのものが「展開完了」を意味する（中途半端な状態が見えることはない）。

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use sea_orm::prelude::Uuid;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::io::ReaderStream;

use super::bundle::{BundleError, extract_and_index};
use super::download_bundle;
use crate::storage::{ByteStream, StorageBackend, StorageError};

/// Storybook 配信で起こりうるエラー。
#[derive(Debug, Error)]
pub enum StorybookServeError {
    /// 要求されたファイルが存在しない、またはパスが安全でない（トラバーサル試行など）。
    /// どちらも呼び出し側では 404 に落とす（存在の有無を漏らさない）。
    #[error("storybook asset not found")]
    NotFound,
    /// バンドルの展開に失敗した（壊れた zip・上限超過など）。
    #[error("bundle extraction failed: {0}")]
    Bundle(#[from] BundleError),
    /// ストレージ read / ローカル IO エラー。
    #[error("storage error: {0}")]
    Storage(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 配信するアセット 1 件。
pub struct StorybookAsset {
    /// 拡張子から決めた Content-Type。
    pub content_type: &'static str,
    /// ファイル本体のストリーム。
    pub stream: ByteStream,
    /// バイト数（Content-Length 用）。
    pub len: u64,
}

/// ビルドごとの展開ロック。プロセス内で同一ビルドの初回展開を直列化する。
static BUILD_LOCKS: LazyLock<Mutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn build_lock(build_id: Uuid) -> Arc<AsyncMutex<()>> {
    let mut map = BUILD_LOCKS.lock().expect("build lock map poisoned");
    map.entry(build_id).or_default().clone()
}

/// ビルドのバンドルがローカルに展開済みであることを保証し、配信のドキュメントルートを返す。
///
/// 既に `{cache_dir}/{build_id}` があればそれを返す。無ければロックを取ってから、
/// ストレージから zip を取得 → 一時ディレクトリへ展開 → ドキュメントルートを最終パスへ
/// `rename` する。
async fn ensure_extracted(
    storage: &Arc<dyn StorageBackend>,
    cache_dir: &Path,
    storybook_key: &str,
    build_id: Uuid,
) -> Result<PathBuf, StorybookServeError> {
    let final_dir = cache_dir.join(build_id.to_string());
    if final_dir.is_dir() {
        return Ok(final_dir);
    }

    let lock = build_lock(build_id);
    let _guard = lock.lock().await;

    // ロック待ちの間に別タスクが展開し終えているかもしれない。
    if final_dir.is_dir() {
        return Ok(final_dir);
    }

    // zip を取得（[`super::MAX_BUNDLE_BYTES`] で打ち切る）。
    let bytes = download_bundle(storage, storybook_key)
        .await
        .map_err(|e| StorybookServeError::Storage(e.to_string()))?;

    tokio::fs::create_dir_all(cache_dir).await?;
    let tmp = cache_dir.join(format!(".tmp-{}", Uuid::new_v4().simple()));

    // 展開は同期 IO（std::fs）なのでブロッキングスレッドへ。
    let tmp_for_extract = tmp.clone();
    let extracted = tokio::task::spawn_blocking(move || {
        extract_and_index(&bytes, &tmp_for_extract).map(|b| b.root)
    })
    .await
    .map_err(|e| StorybookServeError::Storage(format!("extract task join: {e}")));

    let root = match extracted {
        Ok(Ok(root)) => root,
        Ok(Err(bundle_err)) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(StorybookServeError::Bundle(bundle_err));
        }
        Err(join_err) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(join_err);
        }
    };

    // ドキュメントルート（`root`）を最終パスへアトミックに移す。
    // `root` は `tmp` 自身か、その 1 階層下（`storybook-static/` 形式）のどちらか。
    let rename_res = tokio::fs::rename(&root, &final_dir).await;
    // `tmp` が残っていれば掃除する（root == tmp の場合は rename で消えている）。
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    if let Err(e) = rename_res {
        // 別プロセス/タスクが同じ最終パスを先に作った場合（クロスプロセスのレース）は、
        // それが正となる。最終ディレクトリがあるなら成功として扱う。
        if final_dir.is_dir() {
            return Ok(final_dir);
        }
        return Err(StorybookServeError::Io(e));
    }

    Ok(final_dir)
}

/// キャッシュディレクトリ配下の相対パスを、安全な絶対パスへ解決する。
///
/// `..`・絶対パス・Windows 区切り・シンボリックリンクでルート外へ出る経路を拒否する。
/// 解決後のパスは実在するファイルであること（canonicalize が成立し、ルート配下に収まる）。
fn resolve_asset(root: &Path, rel: &str) -> Result<PathBuf, StorybookServeError> {
    // Windows 由来の `\` は `..` を隠す経路になるので拒否。
    if rel.contains('\\') {
        return Err(StorybookServeError::NotFound);
    }

    let mut resolved = root.to_path_buf();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(StorybookServeError::NotFound);
            }
        }
    }

    // canonicalize は実在パスにしか成立しない = 「存在するファイル」の確認を兼ねる。
    // シンボリックリンクもここで解決されるので、リンク先がルート外なら下の接頭辞
    // チェックで弾ける。
    let real = resolved
        .canonicalize()
        .map_err(|_| StorybookServeError::NotFound)?;
    let root_real = root
        .canonicalize()
        .map_err(|_| StorybookServeError::NotFound)?;

    if !real.starts_with(&root_real) {
        return Err(StorybookServeError::NotFound);
    }
    if !real.is_file() {
        return Err(StorybookServeError::NotFound);
    }

    Ok(real)
}

/// 拡張子から Content-Type を決める。未知の拡張子は `application/octet-stream`。
fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

/// ビルドのバンドルから 1 ファイルを配信可能な形で取り出す。
///
/// `rel_path` が空なら `index.html` を配信する（`/storybook/` のインデックス要求）。
/// 初回はここで展開が走る。バンドル未アップロードや壊れた zip は呼び出し側で扱う。
pub async fn serve_asset(
    storage: &Arc<dyn StorageBackend>,
    cache_dir: &Path,
    storybook_key: &str,
    build_id: Uuid,
    rel_path: &str,
) -> Result<StorybookAsset, StorybookServeError> {
    let root = ensure_extracted(storage, cache_dir, storybook_key, build_id).await?;

    let rel = rel_path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

    let file_path = resolve_asset(&root, rel)?;
    let content_type = content_type_for(&file_path);

    let file = tokio::fs::File::open(&file_path).await?;
    let len = file.metadata().await?.len();
    let stream: ByteStream = Box::pin(futures::StreamExt::map(ReaderStream::new(file), |res| {
        res.map_err(StorageError::from)
    }));

    Ok(StorybookAsset {
        content_type,
        stream,
        len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn zip_with(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, contents) in files {
                writer.start_file(*name, options).expect("start file");
                writer.write_all(contents).expect("write");
            }
            writer.finish().expect("finish zip");
        }
        buf.into_inner()
    }

    const INDEX_JSON: &[u8] =
        br#"{"v":5,"entries":{"a--b":{"type":"story","id":"a--b","title":"A","name":"B"}}}"#;

    fn local_storage(dir: &Path) -> Arc<dyn StorageBackend> {
        Arc::new(crate::storage::local::LocalStorageBackend::new(dir))
    }

    #[tokio::test]
    async fn extracts_once_and_serves_index_and_nested_asset() {
        let store_dir = tempfile::tempdir().expect("store dir");
        let cache_dir = tempfile::tempdir().expect("cache dir");
        let storage = local_storage(store_dir.path());
        let build_id = Uuid::new_v4();

        let zip = zip_with(&[
            ("index.json", INDEX_JSON),
            ("index.html", b"<html>manager</html>"),
            ("iframe.html", b"<html>iframe</html>"),
            ("assets/app.js", b"console.log(1)"),
        ]);
        let key = "tenants/t/projects/p/builds/b/storybook.zip";
        crate::render::upload_bundle(&storage, key, bytes::Bytes::from(zip))
            .await
            .expect("upload");

        // index（空パス）
        let asset = serve_asset(&storage, cache_dir.path(), key, build_id, "")
            .await
            .expect("index");
        assert_eq!(asset.content_type, "text/html; charset=utf-8");
        assert_eq!(asset.len, "<html>manager</html>".len() as u64);

        // ネストしたアセット。2 回目なので展開はスキップされる（最終ディレクトリが既にある）。
        let asset = serve_asset(&storage, cache_dir.path(), key, build_id, "assets/app.js")
            .await
            .expect("nested asset");
        assert_eq!(asset.content_type, "text/javascript; charset=utf-8");

        assert!(cache_dir.path().join(build_id.to_string()).is_dir());
    }

    #[tokio::test]
    async fn rejects_traversal_and_missing() {
        let store_dir = tempfile::tempdir().expect("store dir");
        let cache_dir = tempfile::tempdir().expect("cache dir");
        let storage = local_storage(store_dir.path());
        let build_id = Uuid::new_v4();

        let zip = zip_with(&[("index.json", INDEX_JSON), ("index.html", b"<html></html>")]);
        let key = "tenants/t/projects/p/builds/b/storybook.zip";
        crate::render::upload_bundle(&storage, key, bytes::Bytes::from(zip))
            .await
            .expect("upload");

        for bad in ["../../etc/passwd", "..", "/etc/passwd", "a\\b", "nope.js"] {
            match serve_asset(&storage, cache_dir.path(), key, build_id, bad).await {
                Err(StorybookServeError::NotFound) => {}
                Err(other) => panic!("expected NotFound for {bad}, got {other:?}"),
                Ok(_) => panic!("expected rejection for {bad}"),
            }
        }
    }
}
