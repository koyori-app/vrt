//! ローカルディスク上のファイルストレージ。

use std::path::PathBuf;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio_util::io::ReaderStream;

use super::r#trait::{ByteStream, StorageBackend, StorageError};

/// `LOCAL_UPLOAD_DIR` 配下にファイルを保存するバックエンド。
#[derive(Clone, Debug)]
pub struct LocalStorageBackend {
    upload_dir: PathBuf,
}

impl LocalStorageBackend {
    pub fn new(upload_dir: impl Into<PathBuf>) -> Self {
        Self {
            upload_dir: upload_dir.into(),
        }
    }

    fn resolve_path(&self, key: &str) -> Result<PathBuf, StorageError> {
        validate_key(key)?;
        Ok(self.upload_dir.join(key))
    }
}

/// ストレージキーの検証。
///
/// VRT のキーは `tenants/{id}/projects/{id}/builds/{id}/{file}.png` のように
/// 階層を持つため、task の実装（スラッシュ全面禁止）から**スラッシュ区切りを許可**する
/// ように緩めている。代わりにパストラバーサルは以下で塞ぐ:
///
/// - 先頭スラッシュ（絶対パス化）を拒否
/// - `\`（Windows 区切り）を拒否
/// - セグメントが空（`//`・末尾スラッシュ）、`.`、`..` を拒否
fn validate_key(key: &str) -> Result<(), StorageError> {
    if key.is_empty() || key.starts_with('/') || key.contains('\\') {
        return Err(StorageError::InvalidKey);
    }
    for segment in key.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(StorageError::InvalidKey);
        }
    }
    Ok(())
}

#[async_trait]
impl StorageBackend for LocalStorageBackend {
    async fn upload(
        &self,
        key: &str,
        mut stream: ByteStream,
        content_length: u64,
        _mime: &str,
    ) -> Result<(), StorageError> {
        let path = self.resolve_path(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let file = File::create(&path).await?;
        let mut writer = BufWriter::new(file);
        let mut written: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            written += chunk.len() as u64;
            writer.write_all(&chunk).await?;
        }
        writer.flush().await?;

        if content_length > 0 && written != content_length {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(StorageError::SizeMismatch {
                expected: content_length,
                actual: written,
            });
        }

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let path = self.resolve_path(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn get_stream(&self, key: &str) -> Result<ByteStream, StorageError> {
        let path = self.resolve_path(key)?;
        let file = File::open(&path).await?;
        let stream = ReaderStream::new(file).map(|res| res.map_err(StorageError::from));
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_key_rejects_empty() {
        assert!(matches!(validate_key(""), Err(StorageError::InvalidKey)));
    }

    #[test]
    fn validate_key_rejects_absolute_path() {
        assert!(matches!(
            validate_key("/etc/passwd"),
            Err(StorageError::InvalidKey)
        ));
    }

    #[test]
    fn validate_key_rejects_double_dot() {
        assert!(matches!(
            validate_key("../escape"),
            Err(StorageError::InvalidKey)
        ));
        assert!(matches!(validate_key(".."), Err(StorageError::InvalidKey)));
        assert!(matches!(
            validate_key("tenants/../../etc/passwd"),
            Err(StorageError::InvalidKey)
        ));
        assert!(matches!(
            validate_key("tenants/./x"),
            Err(StorageError::InvalidKey)
        ));
    }

    #[test]
    fn validate_key_rejects_empty_segments() {
        assert!(matches!(
            validate_key("tenants//x"),
            Err(StorageError::InvalidKey)
        ));
        assert!(matches!(
            validate_key("tenants/"),
            Err(StorageError::InvalidKey)
        ));
    }

    #[test]
    fn validate_key_rejects_backslash() {
        assert!(matches!(
            validate_key("foo\\bar"),
            Err(StorageError::InvalidKey)
        ));
    }

    #[test]
    fn validate_key_accepts_valid_keys() {
        assert!(validate_key("uuid-v4-key").is_ok());
        assert!(validate_key("a").is_ok());
        assert!(validate_key("abc123-_.").is_ok());
    }

    #[test]
    fn validate_key_accepts_nested_vrt_keys() {
        // VRT のキーは階層構造を持つ（S3 バックエンドと同じ形）。
        assert!(validate_key("prefix/uuid-key").is_ok());
        assert!(validate_key("tenants/2f1e/projects/9a3c/builds/77bd/diffs/0f21.png").is_ok());
    }
}
