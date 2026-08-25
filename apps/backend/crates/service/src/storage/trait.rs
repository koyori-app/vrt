//! ストレージバックエンドの抽象インターフェース。

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use thiserror::Error;

/// ストリーミング読み書き用のバイトストリーム。
pub type ByteStream = BoxStream<'static, Result<Bytes, StorageError>>;

/// ストレージ操作のエラー。
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("upload size mismatch: expected {expected}, wrote {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("invalid storage key")]
    InvalidKey,
    #[error("{0}")]
    Other(String),
}

/// S3 / ローカルディスクを抽象化するストレージバックエンド。
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// ストリーミングアップロード。大ファイルをメモリに全展開しない。
    async fn upload(
        &self,
        key: &str,
        stream: ByteStream,
        content_length: u64,
        mime: &str,
    ) -> Result<(), StorageError>;

    async fn delete(&self, key: &str) -> Result<(), StorageError>;

    /// オブジェクトが存在すればサイズを返す。存在しない場合は`None`。
    ///
    /// local→S3移行では同じサイズのオブジェクトを再送せず、途中から安全に再開するために使う。
    async fn object_size(&self, key: &str) -> Result<Option<u64>, StorageError>;

    /// ストリーミングダウンロード（プロキシ配信用）。
    async fn get_stream(&self, key: &str) -> Result<ByteStream, StorageError>;
}
