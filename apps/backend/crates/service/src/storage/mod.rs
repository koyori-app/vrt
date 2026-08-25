//! ストレージバックエンド（trait + ローカル / S3 実装）。

use std::env;
use std::sync::Arc;

pub mod local;
pub mod migrate;
pub mod s3;
pub mod r#trait;

pub use local::LocalStorageBackend;
pub use migrate::{
    MIGRATION_COMPLETION_MARKER_KEY, MigrationRunOutcome, MigrationSummary,
    migrate_local_directory, migrate_local_directory_once,
};
pub use s3::S3StorageBackend;
pub use r#trait::{ByteStream, StorageBackend, StorageError};

/// `STORAGE_BACKEND` 環境変数に応じてストレージバックエンドを初期化する。
///
/// - `local`（既定）: `LOCAL_UPLOAD_DIR`（未設定時 `./uploads`）
/// - `s3`: `S3_*` 環境変数を `S3StorageBackend::from_env` が読む
pub async fn setup_storage() -> Result<Arc<dyn StorageBackend>, StorageError> {
    let backend = env::var("STORAGE_BACKEND").unwrap_or_else(|_| "local".into());
    match backend.as_str() {
        "s3" => {
            let s3 = S3StorageBackend::from_env().await?;
            Ok(Arc::new(s3))
        }
        "local" => {
            let upload_dir = env::var("LOCAL_UPLOAD_DIR").unwrap_or_else(|_| "./uploads".into());
            Ok(Arc::new(LocalStorageBackend::new(upload_dir)))
        }
        other => Err(StorageError::Other(format!(
            "invalid STORAGE_BACKEND: {other} (expected \"local\" or \"s3\")"
        ))),
    }
}
