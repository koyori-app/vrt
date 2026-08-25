//! `LOCAL_UPLOAD_DIR`の既存成果物をS3互換ストレージへ一括移行するone-shot。

use std::path::PathBuf;

use service::storage::{S3StorageBackend, migrate_local_directory};

const DEFAULT_CONCURRENCY: usize = 4;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    dotenvy::dotenv().ok();
    let backend = std::env::var("STORAGE_BACKEND").unwrap_or_else(|_| "local".into());
    if backend == "local" {
        tracing::info!("STORAGE_BACKEND=local; storage migration is not required");
        return Ok(());
    }
    if backend != "s3" {
        anyhow::bail!("invalid STORAGE_BACKEND: {backend} (expected `local` or `s3`)");
    }

    let source =
        PathBuf::from(std::env::var("LOCAL_UPLOAD_DIR").unwrap_or_else(|_| "./uploads".into()));
    let concurrency = migration_concurrency()?;
    let destination = S3StorageBackend::from_env()
        .await
        .map_err(|error| anyhow::anyhow!("initialize S3 destination: {error}"))?;

    tracing::info!(
        source = %source.display(),
        concurrency,
        "starting local-to-S3 storage migration"
    );
    let summary = migrate_local_directory(&source, &destination, concurrency)
        .await
        .map_err(|error| anyhow::anyhow!("local-to-S3 storage migration failed: {error}"))?;
    tracing::info!(
        discovered = summary.discovered,
        uploaded = summary.uploaded,
        skipped = summary.skipped,
        bytes_uploaded = summary.bytes_uploaded,
        "local-to-S3 storage migration completed"
    );
    Ok(())
}

fn migration_concurrency() -> Result<usize, anyhow::Error> {
    let raw = std::env::var("STORAGE_MIGRATION_CONCURRENCY").ok();
    parse_migration_concurrency(raw.as_deref())
}

fn parse_migration_concurrency(raw: Option<&str>) -> Result<usize, anyhow::Error> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_CONCURRENCY);
    };
    let parsed = raw.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("STORAGE_MIGRATION_CONCURRENCY must be a positive integer, got `{raw}`")
    })?;
    if parsed == 0 {
        anyhow::bail!("STORAGE_MIGRATION_CONCURRENCY must be greater than zero");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_defaults_and_validates() {
        assert_eq!(parse_migration_concurrency(None).unwrap(), 4);
        assert_eq!(parse_migration_concurrency(Some("8")).unwrap(), 8);
        assert!(parse_migration_concurrency(Some("0")).is_err());
        assert!(parse_migration_concurrency(Some("invalid")).is_err());
    }
}
