//! Storybook バンドルのサーバーサイドレンダリング。
//!
//! `mode = storybook` のビルドは、CI が撮ったスクリーンショットではなく
//! **ビルド済み Storybook の zip** を受け取り、こちらでヘッドレス Chromium を
//! 回してストーリーを 1 枚ずつ撮る（Chromatic 方式）。
//!
//! - [`bundle`]: zip の安全な展開と `index.json` からのストーリー抽出
//! - [`browser`]: 展開結果のループバック配信 + Chromium での撮影
//!
//! 実際にジョブとして駆動するのは `job::render_build`。

pub mod browser;
pub mod bundle;

pub use browser::{
    DEFAULT_STORY_TIMEOUT, RenderError, RenderOptions, StaticServer, StoryRenderer,
    discover_chromium, story_url,
};
pub use bundle::{
    BundleError, ExtractLimits, ExtractedBundle, MAX_BUNDLE_BYTES, MAX_ENTRIES,
    MAX_UNCOMPRESSED_BYTES, Story, extract_and_index, extract_zip, extract_zip_with_limits,
    locate_index, parse_index,
};

/// storybook バンドルのストレージキー。1 ビルドにつき 1 本だけ。
pub fn storybook_key(
    tenant_id: sea_orm::prelude::Uuid,
    project_id: sea_orm::prelude::Uuid,
    build_id: sea_orm::prelude::Uuid,
) -> String {
    format!("tenants/{tenant_id}/projects/{project_id}/builds/{build_id}/storybook.zip")
}

/// zip の Content-Type。
pub const ZIP_MIME: &str = "application/zip";

/// zip のローカルファイルヘッダのシグネチャ。
pub const ZIP_MAGIC: [u8; 4] = [b'P', b'K', 0x03, 0x04];

/// アップロードされたバイト列が zip かどうかを検証する。
pub fn validate_zip(bytes: &[u8]) -> Result<(), common::error::AppError> {
    use common::error::AppError;

    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(AppError::ContentTooLarge);
    }
    if bytes.len() < ZIP_MAGIC.len() || bytes[..ZIP_MAGIC.len()] != ZIP_MAGIC {
        return Err(AppError::BadRequestDetail(
            "file is not a zip archive".into(),
        ));
    }
    Ok(())
}

/// バンドル zip をストレージへ保存する。
pub async fn upload_bundle(
    storage: &std::sync::Arc<dyn crate::storage::StorageBackend>,
    key: &str,
    bytes: bytes::Bytes,
) -> Result<(), crate::storage::StorageError> {
    let len = bytes.len() as u64;
    let stream = Box::pin(futures::stream::once(async move { Ok(bytes) }));
    storage.upload(key, stream, len, ZIP_MIME).await
}

/// バンドル zip をストレージから読み出す（[`MAX_BUNDLE_BYTES`] で打ち切る）。
pub async fn download_bundle(
    storage: &std::sync::Arc<dyn crate::storage::StorageBackend>,
    key: &str,
) -> Result<Vec<u8>, anyhow::Error> {
    use futures::StreamExt;

    let mut stream = storage
        .get_stream(key)
        .await
        .map_err(|e| anyhow::anyhow!("read {key}: {e}"))?;
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("read {key}: {e}"))?;
        if buf.len() + chunk.len() > MAX_BUNDLE_BYTES {
            return Err(anyhow::anyhow!(
                "storybook bundle {key} exceeds {MAX_BUNDLE_BYTES} bytes"
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::prelude::Uuid;

    #[test]
    fn storybook_keys_live_under_the_build_prefix() {
        let tenant = Uuid::new_v4();
        let project = Uuid::new_v4();
        let build = Uuid::new_v4();
        let key = storybook_key(tenant, project, build);

        assert_eq!(
            key,
            format!("tenants/{tenant}/projects/{project}/builds/{build}/storybook.zip")
        );
        // ローカルバックエンドのキー検証を通ること。
        assert!(!key.contains(".."));
        assert!(!key.starts_with('/'));
    }

    #[test]
    fn zip_validation_checks_magic_and_size() {
        assert!(validate_zip(b"PK\x03\x04rest of the archive").is_ok());
        assert!(validate_zip(b"not a zip at all").is_err());
        assert!(validate_zip(b"PK").is_err());
        assert!(validate_zip(&[]).is_err());
    }
}
