//! スクリーンショット（PNG）の検証・保存・読み出し。
//!
//! ## ストレージキー
//!
//! ```text
//! tenants/{tenant_id}/projects/{project_id}/builds/{build_id}/{screenshot_id}.png
//! tenants/{tenant_id}/projects/{project_id}/builds/{build_id}/diffs/{comparison_id}.png
//! ```
//!
//! baseline エントリは昇格元スクリーンショットのキーをそのまま参照する（コピーしない）。
//! ビルドを消すと baseline の実体も消えるため、ビルドの物理削除は行わない前提。

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use image::{ImageFormat, RgbaImage};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, prelude::Uuid,
};

use common::error::AppError;
use entity::screenshots;

use crate::storage::{ByteStream, StorageBackend, StorageError};

/// 1 枚あたりのアップロード上限（25MB）。
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
/// 許容する最大寸法（幅・高さとも）。diff ジョブのメモリを保護する。
pub const MAX_DIMENSION: u32 = 10_000;
/// PNG のシグネチャ。
pub const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// 配信時の Content-Type。
pub const PNG_MIME: &str = "image/png";

/// スクリーンショット本体のストレージキー。
pub fn screenshot_key(
    tenant_id: Uuid,
    project_id: Uuid,
    build_id: Uuid,
    screenshot_id: Uuid,
) -> String {
    format!("tenants/{tenant_id}/projects/{project_id}/builds/{build_id}/{screenshot_id}.png")
}

/// 差分画像のストレージキー。
pub fn diff_key(tenant_id: Uuid, project_id: Uuid, build_id: Uuid, comparison_id: Uuid) -> String {
    format!("tenants/{tenant_id}/projects/{project_id}/builds/{build_id}/diffs/{comparison_id}.png")
}

/// PNG のマジックバイトと寸法を検証する。デコードできない画像はここで弾く。
pub fn validate_png(bytes: &[u8]) -> Result<(u32, u32), AppError> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(AppError::ContentTooLarge);
    }
    if bytes.len() < PNG_MAGIC.len() || bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
        return Err(AppError::BadRequestDetail("file is not a PNG".into()));
    }

    let reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), ImageFormat::Png);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| AppError::BadRequestDetail(format!("invalid PNG: {e}")))?;

    if width == 0 || height == 0 {
        return Err(AppError::BadRequestDetail(
            "image dimensions must be positive".into(),
        ));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(AppError::BadRequestDetail(format!(
            "image is too large: {width}x{height} (max {MAX_DIMENSION}x{MAX_DIMENSION})"
        )));
    }

    Ok((width, height))
}

/// 1 ショットぶんのバイト列をそのままストリームにする。
fn one_shot_stream(bytes: Bytes) -> ByteStream {
    Box::pin(futures::stream::once(async move { Ok(bytes) }))
}

/// PNG をストレージへ保存する。
pub async fn upload_png(
    storage: &Arc<dyn StorageBackend>,
    key: &str,
    bytes: Bytes,
) -> Result<(), StorageError> {
    let len = bytes.len() as u64;
    storage
        .upload(key, one_shot_stream(bytes), len, PNG_MIME)
        .await
}

/// ストレージから PNG を読み出して RGBA8 にデコードする。
pub async fn load_rgba(
    storage: &Arc<dyn StorageBackend>,
    key: &str,
) -> Result<RgbaImage, anyhow::Error> {
    let bytes = read_all(storage, key).await?;
    let image = image::ImageReader::with_format(std::io::Cursor::new(&bytes), ImageFormat::Png)
        .decode()
        .map_err(|e| anyhow::anyhow!("decode png {key}: {e}"))?;
    Ok(image.to_rgba8())
}

/// ストレージのオブジェクトを全部メモリに読み出す。
pub async fn read_all(
    storage: &Arc<dyn StorageBackend>,
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
        if buf.len() + chunk.len() > MAX_UPLOAD_BYTES {
            return Err(anyhow::anyhow!(
                "object {key} exceeds {MAX_UPLOAD_BYTES} bytes"
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// `RgbaImage` を PNG にエンコードする。
pub fn encode_png(image: &RgbaImage) -> Result<Bytes, anyhow::Error> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| anyhow::anyhow!("encode png: {e}"))?;
    Ok(Bytes::from(buf.into_inner()))
}

/// アップロードされた PNG を検証し、ストレージへ保存して行を作る。
///
/// 同名のスクリーンショットが既にあれば [`AppError::Conflict`]（DB の UNIQUE 制約も保険）。
pub async fn store_screenshot<C: ConnectionTrait>(
    db: &C,
    storage: &Arc<dyn StorageBackend>,
    tenant_id: Uuid,
    project_id: Uuid,
    build_id: Uuid,
    name: String,
    bytes: Bytes,
) -> Result<screenshots::Model, AppError> {
    store_screenshot_with_metadata(
        db, storage, tenant_id, project_id, build_id, name, bytes, None,
    )
    .await
}

/// [`store_screenshot`] と同じだが、`metadata` を添えて保存する。
///
/// storybook モードのレンダリングが `{"story_id": ..., "title": ...}` を入れる。
#[allow(clippy::too_many_arguments)]
pub async fn store_screenshot_with_metadata<C: ConnectionTrait>(
    db: &C,
    storage: &Arc<dyn StorageBackend>,
    tenant_id: Uuid,
    project_id: Uuid,
    build_id: Uuid,
    name: String,
    bytes: Bytes,
    metadata: Option<serde_json::Value>,
) -> Result<screenshots::Model, AppError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::BadRequestDetail("name is required".into()));
    }
    if name.len() > 255 {
        return Err(AppError::BadRequestDetail(
            "name must be 255 characters or fewer".into(),
        ));
    }

    let (width, height) = validate_png(&bytes)?;

    let duplicate = screenshots::Entity::find()
        .filter(screenshots::Column::BuildId.eq(build_id))
        .filter(screenshots::Column::Name.eq(name.clone()))
        .one(db)
        .await?;
    if duplicate.is_some() {
        return Err(AppError::Conflict);
    }

    let screenshot_id = Uuid::new_v4();
    let key = screenshot_key(tenant_id, project_id, build_id, screenshot_id);
    upload_png(storage, &key, bytes)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("upload screenshot: {e}")))?;

    Ok(screenshots::ActiveModel {
        id: Set(screenshot_id),
        build_id: Set(build_id),
        name: Set(name),
        storage_key: Set(key),
        width: Set(width as i32),
        height: Set(height as i32),
        metadata: Set(metadata),
        created_at: Set(Utc::now().fixed_offset()),
    }
    .insert(db)
    .await?)
}

/// ビルドのスクリーンショット一覧（名前順）。
pub async fn list_for_build<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
) -> Result<Vec<screenshots::Model>, AppError> {
    Ok(screenshots::Entity::find()
        .filter(screenshots::Column::BuildId.eq(build_id))
        .order_by_asc(screenshots::Column::Name)
        .all(db)
        .await?)
}

/// スクリーンショットを ID で取得する。
pub async fn get_screenshot<C: ConnectionTrait>(
    db: &C,
    screenshot_id: Uuid,
) -> Result<screenshots::Model, AppError> {
    screenshots::Entity::find_by_id(screenshot_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// 画像配信用のストリームを開く。
pub async fn open_stream(
    storage: &Arc<dyn StorageBackend>,
    key: &str,
) -> Result<ByteStream, AppError> {
    storage.get_stream(key).await.map_err(|e| match e {
        StorageError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound => AppError::NotFound,
        StorageError::InvalidKey => AppError::NotFound,
        other => AppError::Internal(anyhow::anyhow!("open storage stream: {other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn png_bytes(width: u32, height: u32) -> Bytes {
        let image = RgbaImage::from_pixel(width, height, Rgba([1, 2, 3, 255]));
        encode_png(&image).expect("encode")
    }

    #[test]
    fn accepts_valid_png() {
        let bytes = png_bytes(12, 7);
        assert_eq!(validate_png(&bytes).expect("valid"), (12, 7));
    }

    #[test]
    fn rejects_non_png_magic() {
        let err = validate_png(b"GIF89a and some more bytes").unwrap_err();
        assert!(matches!(err, AppError::BadRequestDetail(_)));
    }

    #[test]
    fn rejects_truncated_input() {
        assert!(validate_png(&[0x89, b'P']).is_err());
        assert!(validate_png(&[]).is_err());
    }

    #[test]
    fn rejects_png_magic_with_corrupt_body() {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend_from_slice(&[0u8; 32]);
        assert!(validate_png(&bytes).is_err());
    }

    #[test]
    fn rejects_oversized_payload() {
        let bytes = vec![0u8; MAX_UPLOAD_BYTES + 1];
        assert!(matches!(
            validate_png(&bytes).unwrap_err(),
            AppError::ContentTooLarge
        ));
    }

    #[test]
    fn storage_keys_are_nested_and_distinct() {
        let tenant = Uuid::new_v4();
        let project = Uuid::new_v4();
        let build = Uuid::new_v4();
        let shot = Uuid::new_v4();
        let comparison = Uuid::new_v4();

        let shot_key = screenshot_key(tenant, project, build, shot);
        let diff = diff_key(tenant, project, build, comparison);

        assert!(shot_key.starts_with(&format!(
            "tenants/{tenant}/projects/{project}/builds/{build}/"
        )));
        assert!(shot_key.ends_with(".png"));
        assert!(diff.contains("/diffs/"));
        assert_ne!(shot_key, diff);
        // ローカルバックエンドのキー検証を通ること（パストラバーサル無し）。
        assert!(!shot_key.contains(".."));
        assert!(!shot_key.starts_with('/'));
    }

    #[test]
    fn png_roundtrips_through_encode() {
        let image = RgbaImage::from_pixel(3, 4, Rgba([9, 8, 7, 255]));
        let bytes = encode_png(&image).expect("encode");
        assert_eq!(validate_png(&bytes).expect("valid"), (3, 4));
    }
}
