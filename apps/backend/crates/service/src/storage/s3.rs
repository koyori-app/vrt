//! S3 互換ストレージバックエンド（AWS S3 / MinIO 等）。

use std::env;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::{
    Attribute, Attributes, ObjectStore, PutMultipartOpts, PutOptions, PutPayload, path::Path,
};

use super::r#trait::{ByteStream, StorageBackend, StorageError};

const MULTIPART_THRESHOLD: u64 = 5 * 1024 * 1024;
const MULTIPART_PART_SIZE: usize = 5 * 1024 * 1024;

/// S3 互換 API へオブジェクトを保存するバックエンド。
#[derive(Clone)]
pub struct S3StorageBackend {
    store: Arc<dyn ObjectStore>,
    bucket: String,
    #[allow(dead_code)]
    public_base_url: String,
}

impl std::fmt::Debug for S3StorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3StorageBackend")
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

impl S3StorageBackend {
    /// 環境変数から設定を読み込んでインスタンスを生成する。
    ///
    /// 必須: `S3_ENDPOINT`, `S3_BUCKET`, `S3_REGION`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`
    /// 任意: `S3_PUBLIC_BASE_URL`, `S3_FORCE_PATH_STYLE`（デフォルト false）
    pub async fn from_env() -> Result<Self, StorageError> {
        let endpoint = env::var("S3_ENDPOINT")
            .map_err(|_| StorageError::Other("S3_ENDPOINT is not set".into()))?;
        let bucket = env::var("S3_BUCKET")
            .map_err(|_| StorageError::Other("S3_BUCKET is not set".into()))?;
        let region = env::var("S3_REGION")
            .map_err(|_| StorageError::Other("S3_REGION is not set".into()))?;
        let access_key_id = env::var("S3_ACCESS_KEY_ID")
            .map_err(|_| StorageError::Other("S3_ACCESS_KEY_ID is not set".into()))?;
        let secret_access_key = env::var("S3_SECRET_ACCESS_KEY")
            .map_err(|_| StorageError::Other("S3_SECRET_ACCESS_KEY is not set".into()))?;

        let force_path_style = env::var("S3_FORCE_PATH_STYLE")
            .ok()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        let public_base_url = env::var("S3_PUBLIC_BASE_URL").unwrap_or_else(|_| {
            format!(
                "{}/{}",
                endpoint.trim_end_matches('/'),
                bucket.trim_start_matches('/')
            )
        });

        let mut builder = AmazonS3Builder::new()
            .with_endpoint(&endpoint)
            .with_bucket_name(&bucket)
            .with_region(&region)
            .with_access_key_id(&access_key_id)
            .with_secret_access_key(&secret_access_key)
            .with_allow_http(endpoint.starts_with("http://"));

        if force_path_style {
            builder = builder.with_virtual_hosted_style_request(false);
        }

        let store = builder
            .build()
            .map_err(|e| StorageError::Other(format!("S3 client build failed: {e}")))?;

        Ok(Self {
            store: Arc::new(store),
            bucket,
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
        })
    }
}

fn to_path(key: &str) -> Result<Path, StorageError> {
    if key.is_empty() {
        return Err(StorageError::InvalidKey);
    }
    Path::parse(key).map_err(|_| StorageError::InvalidKey)
}

fn mime_attributes(mime: &str) -> Attributes {
    let mut attrs = Attributes::new();
    attrs.insert(Attribute::ContentType, mime.to_string().into());
    attrs
}

#[async_trait]
impl StorageBackend for S3StorageBackend {
    async fn upload(
        &self,
        key: &str,
        mut stream: ByteStream,
        content_length: u64,
        mime: &str,
    ) -> Result<(), StorageError> {
        let path = to_path(key)?;

        if content_length < MULTIPART_THRESHOLD {
            let mut buffer = Vec::with_capacity(content_length as usize);
            let mut read: u64 = 0;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                read += chunk.len() as u64;
                buffer.extend_from_slice(&chunk);
            }
            if content_length > 0 && read != content_length {
                return Err(StorageError::SizeMismatch {
                    expected: content_length,
                    actual: read,
                });
            }

            let opts = PutOptions {
                attributes: mime_attributes(mime),
                ..Default::default()
            };
            self.store
                .put_opts(&path, PutPayload::from(Bytes::from(buffer)), opts)
                .await
                .map_err(|e| StorageError::Other(format!("S3 PutObject failed: {e}")))?;
        } else {
            let opts = PutMultipartOpts {
                attributes: mime_attributes(mime),
                ..Default::default()
            };
            let mut upload = self
                .store
                .put_multipart_opts(&path, opts)
                .await
                .map_err(|e| {
                    StorageError::Other(format!("S3 CreateMultipartUpload failed: {e}"))
                })?;

            let mut pending: Vec<u8> = Vec::with_capacity(MULTIPART_PART_SIZE);
            let mut uploaded: u64 = 0;

            let result: Result<(), StorageError> = async {
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    uploaded += chunk.len() as u64;
                    pending.extend_from_slice(&chunk);

                    while pending.len() >= MULTIPART_PART_SIZE {
                        let part =
                            Bytes::from(pending.drain(..MULTIPART_PART_SIZE).collect::<Vec<_>>());
                        upload.put_part(PutPayload::from(part)).await.map_err(|e| {
                            StorageError::Other(format!("S3 UploadPart failed: {e}"))
                        })?;
                    }
                }
                if !pending.is_empty() {
                    upload
                        .put_part(PutPayload::from(Bytes::from(std::mem::take(&mut pending))))
                        .await
                        .map_err(|e| StorageError::Other(format!("S3 UploadPart failed: {e}")))?;
                }
                if content_length > 0 && uploaded != content_length {
                    return Err(StorageError::SizeMismatch {
                        expected: content_length,
                        actual: uploaded,
                    });
                }
                Ok(())
            }
            .await;

            if result.is_err() {
                let _ = upload.abort().await;
                return result;
            }

            upload.complete().await.map_err(|e| {
                StorageError::Other(format!("S3 CompleteMultipartUpload failed: {e}"))
            })?;
        }

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let path = to_path(key)?;
        self.store
            .delete(&path)
            .await
            .map_err(|e| StorageError::Other(format!("S3 DeleteObject failed: {e}")))?;
        Ok(())
    }

    async fn get_stream(&self, key: &str) -> Result<ByteStream, StorageError> {
        let path = to_path(key)?;
        let result = self
            .store
            .get(&path)
            .await
            .map_err(|e| StorageError::Other(format!("S3 GetObject failed: {e}")))?;

        let stream = result.into_stream().map(|chunk| {
            chunk.map_err(|e| StorageError::Other(format!("S3 GetObject stream failed: {e}")))
        });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // to_path: Path::parse は先頭スラッシュを正規化、バックスラッシュをパーセントエンコードするため
    // 拒否するのは空文字と ".." を含むパスのみ。

    #[test]
    fn to_path_rejects_empty() {
        assert!(matches!(to_path(""), Err(StorageError::InvalidKey)));
    }

    #[test]
    fn to_path_rejects_double_dot() {
        assert!(matches!(
            to_path("../escape"),
            Err(StorageError::InvalidKey)
        ));
        assert!(matches!(
            to_path("foo/../bar"),
            Err(StorageError::InvalidKey)
        ));
        assert!(matches!(to_path(".."), Err(StorageError::InvalidKey)));
    }

    #[test]
    fn to_path_accepts_valid_keys() {
        assert!(to_path("uuid-v4-key").is_ok());
        assert!(to_path("prefix/uuid-key").is_ok());
        assert!(to_path("a").is_ok());
        assert!(to_path("abc123-_.").is_ok());
    }

    /// upload / delete / get_stream が to_path 経由で不正な key を拒否することを確認する。
    /// InMemory ストアを使うことでネットワークアクセスなしにテストできる。
    fn dummy_backend() -> S3StorageBackend {
        S3StorageBackend {
            store: Arc::new(object_store::memory::InMemory::new()),
            bucket: "test".into(),
            public_base_url: "http://localhost:1/test".into(),
        }
    }

    #[test]
    fn upload_rejects_invalid_key() {
        let backend = dummy_backend();
        let stream: ByteStream = Box::pin(futures::stream::empty());
        let result = futures::executor::block_on(backend.upload("", stream, 0, "text/plain"));
        assert!(matches!(result, Err(StorageError::InvalidKey)));
    }

    #[test]
    fn delete_rejects_invalid_key() {
        let backend = dummy_backend();
        let result = futures::executor::block_on(backend.delete("../escape"));
        assert!(matches!(result, Err(StorageError::InvalidKey)));
    }

    #[test]
    fn get_stream_rejects_invalid_key() {
        let backend = dummy_backend();
        let result = futures::executor::block_on(backend.get_stream(""));
        assert!(matches!(result, Err(StorageError::InvalidKey)));
    }
}
