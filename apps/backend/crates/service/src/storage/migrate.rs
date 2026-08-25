//! ローカルストレージからS3互換ストレージへの一括移行。

use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use futures::{StreamExt, stream};
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use super::{ByteStream, StorageBackend, StorageError};

/// このオブジェクトが存在する場合、local→S3移行は完了済みとして全件走査を省略する。
///
/// キーにバージョンを含めることで、将来別形式の移行が必要になった場合はv2へ切り替えられる。
pub const MIGRATION_COMPLETION_MARKER_KEY: &str = ".vrt/migrations/local-to-s3-v1.complete";
const MIGRATION_COMPLETION_MARKER_CONTENT: &[u8] = b"vrt local-to-s3 migration v1 complete\n";

/// 1回の移行結果。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MigrationSummary {
    pub discovered: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub bytes_uploaded: u64,
}

/// 完了マーカー付き移行の実行結果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationRunOutcome {
    /// この実行で全ファイルを移行し、完了マーカーを作成した。
    Completed(MigrationSummary),
    /// 完了マーカーが既に存在したため、ファイル走査を行わなかった。
    AlreadyCompleted,
}

enum FileOutcome {
    Uploaded(u64),
    Skipped,
}

/// `destination`の完了マーカーを確認し、未完了の場合だけlocal→S3移行を実行する。
///
/// 完了マーカーは全ファイルのuploadとサイズ検証が成功した後にだけ作成する。
/// 途中で失敗した場合はマーカーが作られないため、次回の実行で安全に再開できる。
pub async fn migrate_local_directory_once(
    source_root: &Path,
    destination: &dyn StorageBackend,
    concurrency: usize,
) -> Result<MigrationRunOutcome, StorageError> {
    let marker_size = MIGRATION_COMPLETION_MARKER_CONTENT.len() as u64;
    match destination
        .object_size(MIGRATION_COMPLETION_MARKER_KEY)
        .await?
    {
        Some(size) if size == marker_size => return Ok(MigrationRunOutcome::AlreadyCompleted),
        Some(size) => {
            return Err(StorageError::Other(format!(
                "storage migration completion marker has unexpected size: expected {marker_size} bytes, got {size}"
            )));
        }
        None => {}
    }

    let summary = migrate_local_directory(source_root, destination, concurrency).await?;
    let marker = Bytes::from_static(MIGRATION_COMPLETION_MARKER_CONTENT);
    let stream: ByteStream = Box::pin(stream::once(async move { Ok(marker) }));
    destination
        .upload(
            MIGRATION_COMPLETION_MARKER_KEY,
            stream,
            marker_size,
            "text/plain",
        )
        .await?;

    let uploaded_size = destination
        .object_size(MIGRATION_COMPLETION_MARKER_KEY)
        .await?;
    if uploaded_size != Some(marker_size) {
        return Err(StorageError::Other(format!(
            "storage migration completion marker verification failed: expected {marker_size} bytes, got {uploaded_size:?}"
        )));
    }

    Ok(MigrationRunOutcome::Completed(summary))
}

/// `source_root`配下の全ファイルを、相対パスをキーとして`destination`へ移す。
///
/// destinationに同じキー・同じサイズのオブジェクトが既にあればskipするため、
/// 途中で失敗しても同じコマンドを再実行して続きから再開できる。sourceは削除しない。
pub async fn migrate_local_directory(
    source_root: &Path,
    destination: &dyn StorageBackend,
    concurrency: usize,
) -> Result<MigrationSummary, StorageError> {
    let files = collect_files(source_root).await?;
    let discovered = files.len();
    let concurrency = concurrency.max(1);

    let mut outcomes = stream::iter(
        files
            .into_iter()
            .map(|path| async move { migrate_file(source_root, &path, destination).await }),
    )
    .buffer_unordered(concurrency);

    let mut summary = MigrationSummary {
        discovered,
        ..MigrationSummary::default()
    };
    while let Some(outcome) = outcomes.next().await {
        match outcome? {
            FileOutcome::Uploaded(bytes) => {
                summary.uploaded += 1;
                summary.bytes_uploaded = summary.bytes_uploaded.saturating_add(bytes);
            }
            FileOutcome::Skipped => summary.skipped += 1,
        }
    }

    Ok(summary)
}

async fn collect_files(root: &Path) -> Result<Vec<PathBuf>, StorageError> {
    let root_metadata = match tokio::fs::symlink_metadata(root).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StorageError::Io(error)),
    };
    if !root_metadata.is_dir() {
        return Err(StorageError::Other(format!(
            "LOCAL_UPLOAD_DIR is not a directory: {}",
            root.display()
        )));
    }

    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        let mut entries = tokio::fs::read_dir(&directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let path = entry.path();
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file() {
                files.push(path);
            } else {
                return Err(StorageError::Other(format!(
                    "refusing to migrate symlink or special file: {}",
                    path.display()
                )));
            }
        }
    }
    files.sort();
    Ok(files)
}

async fn migrate_file(
    root: &Path,
    path: &Path,
    destination: &dyn StorageBackend,
) -> Result<FileOutcome, StorageError> {
    let key = relative_key(root, path)?;
    let metadata = tokio::fs::metadata(path).await?;
    let size = metadata.len();

    if destination.object_size(&key).await? == Some(size) {
        tracing::debug!(%key, bytes = size, "storage migration skipped existing object");
        return Ok(FileOutcome::Skipped);
    }

    let file = File::open(path).await?;
    let stream: ByteStream =
        Box::pin(ReaderStream::new(file).map(|result| result.map_err(StorageError::from)));
    destination
        .upload(&key, stream, size, content_type(path))
        .await?;

    let uploaded_size = destination.object_size(&key).await?;
    if uploaded_size != Some(size) {
        return Err(StorageError::Other(format!(
            "storage migration verification failed for `{key}`: expected {size} bytes, got {uploaded_size:?}"
        )));
    }

    tracing::info!(%key, bytes = size, "storage migration uploaded object");
    Ok(FileOutcome::Uploaded(size))
}

fn relative_key(root: &Path, path: &Path) -> Result<String, StorageError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        StorageError::Other(format!(
            "migration path `{}` is outside source root `{}`",
            path.display(),
            root.display()
        ))
    })?;

    let mut segments = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => segments.push(segment.to_str().ok_or_else(|| {
                StorageError::Other(format!(
                    "migration path is not valid UTF-8: {}",
                    path.display()
                ))
            })?),
            _ => {
                return Err(StorageError::Other(format!(
                    "invalid migration path: {}",
                    path.display()
                )));
            }
        }
    }
    if segments.is_empty() {
        return Err(StorageError::InvalidKey);
    }
    Ok(segments.join("/"))
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("png") => "image/png",
        Some(extension) if extension.eq_ignore_ascii_case("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::LocalStorageBackend;

    #[tokio::test]
    async fn migration_preserves_keys_and_resumes_by_size() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let nested = source.path().join("tenants/t1/projects/p1/builds/b1");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(nested.join("shot.png"), b"png-data")
            .await
            .unwrap();
        tokio::fs::write(nested.join("storybook.zip"), b"zip-data")
            .await
            .unwrap();
        let backend = LocalStorageBackend::new(destination.path());

        let first = migrate_local_directory(source.path(), &backend, 2)
            .await
            .unwrap();
        assert_eq!(
            first,
            MigrationSummary {
                discovered: 2,
                uploaded: 2,
                skipped: 0,
                bytes_uploaded: 16,
            }
        );
        assert_eq!(
            tokio::fs::read(
                destination
                    .path()
                    .join("tenants/t1/projects/p1/builds/b1/shot.png")
            )
            .await
            .unwrap(),
            b"png-data"
        );
        assert_eq!(
            tokio::fs::read(nested.join("shot.png")).await.unwrap(),
            b"png-data",
            "migration must never modify or delete the source"
        );

        let second = migrate_local_directory(source.path(), &backend, 2)
            .await
            .unwrap();
        assert_eq!(second.discovered, 2);
        assert_eq!(second.uploaded, 0);
        assert_eq!(second.skipped, 2);
        assert_eq!(second.bytes_uploaded, 0);
    }

    #[tokio::test]
    async fn completed_migration_is_not_scanned_or_run_again() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        tokio::fs::write(source.path().join("first.png"), b"first")
            .await
            .unwrap();
        let backend = LocalStorageBackend::new(destination.path());

        let first = migrate_local_directory_once(source.path(), &backend, 2)
            .await
            .unwrap();
        assert_eq!(
            first,
            MigrationRunOutcome::Completed(MigrationSummary {
                discovered: 1,
                uploaded: 1,
                skipped: 0,
                bytes_uploaded: 5,
            })
        );
        assert_eq!(
            backend
                .object_size(MIGRATION_COMPLETION_MARKER_KEY)
                .await
                .unwrap(),
            Some(MIGRATION_COMPLETION_MARKER_CONTENT.len() as u64)
        );

        // 完了後に移行元へファイルを足しても、次回はマーカー確認だけで終了する。
        tokio::fs::write(source.path().join("added-later.png"), b"later")
            .await
            .unwrap();
        let second = migrate_local_directory_once(source.path(), &backend, 2)
            .await
            .unwrap();
        assert_eq!(second, MigrationRunOutcome::AlreadyCompleted);
        assert_eq!(backend.object_size("added-later.png").await.unwrap(), None);
    }

    #[tokio::test]
    async fn failed_migration_does_not_create_completion_marker() {
        let source_file = tempfile::NamedTempFile::new().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let backend = LocalStorageBackend::new(destination.path());

        let result = migrate_local_directory_once(source_file.path(), &backend, 1).await;

        assert!(result.is_err());
        assert_eq!(
            backend
                .object_size(MIGRATION_COMPLETION_MARKER_KEY)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn migration_replaces_an_existing_object_with_a_different_size() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        tokio::fs::write(source.path().join("shot.png"), b"new-image")
            .await
            .unwrap();
        tokio::fs::write(destination.path().join("shot.png"), b"old")
            .await
            .unwrap();
        let backend = LocalStorageBackend::new(destination.path());

        let summary = migrate_local_directory(source.path(), &backend, 1)
            .await
            .unwrap();

        assert_eq!(summary.uploaded, 1);
        assert_eq!(summary.skipped, 0);
        assert_eq!(
            tokio::fs::read(destination.path().join("shot.png"))
                .await
                .unwrap(),
            b"new-image"
        );
    }

    #[tokio::test]
    async fn missing_source_directory_is_an_empty_success() {
        let destination = tempfile::tempdir().unwrap();
        let backend = LocalStorageBackend::new(destination.path());

        let summary = migrate_local_directory(&destination.path().join("missing"), &backend, 1)
            .await
            .unwrap();

        assert_eq!(summary, MigrationSummary::default());
    }

    #[test]
    fn migration_content_types_cover_stored_artifacts() {
        assert_eq!(content_type(Path::new("a.PNG")), "image/png");
        assert_eq!(content_type(Path::new("storybook.zip")), "application/zip");
        assert_eq!(
            content_type(Path::new("unknown")),
            "application/octet-stream"
        );
    }
}
