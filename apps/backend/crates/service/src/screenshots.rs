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
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, prelude::Uuid,
};
use sha2::{Digest, Sha256};

use common::db::with_transaction;
use common::error::AppError;
use common::validation::ScreenshotName;
use entity::{
    builds::{BuildMode, BuildStatus},
    screenshots,
};

use crate::storage::{ByteStream, StorageBackend, StorageError};

/// 1 枚あたりのアップロード上限（25MB）。
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
/// 許容する最大寸法（幅・高さとも）。diff ジョブのメモリを保護する。
pub const MAX_DIMENSION: u32 = 10_000;
/// PNG のシグネチャ。
pub const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// 配信時の Content-Type。
pub const PNG_MIME: &str = "image/png";
/// 保存する content hash の方式。値にも方式を含め、将来の方式変更時に
/// 異なる方式同士を一致扱いしない（判断不能なら通常比較へ倒す）。
pub const CONTENT_HASH_SCHEME: &str = "sha256";

/// 受領した PNG の**バイト列**から content hash を作る。
///
/// デコード後ピクセルを hash すると、hash 判定の前に高コストな decode が必要になり
/// この最適化の目的を失う。さらに見た目が同じでもエンコードが異なる PNG は一致扱いせず、
/// 従来比較へ倒すことで偽 PASS を避ける。
pub fn content_hash(bytes: &[u8]) -> String {
    format!(
        "{CONTENT_HASH_SCHEME}:{}",
        hex::encode(Sha256::digest(bytes))
    )
}

/// 両側に既知方式の hash が揃い、完全一致したときだけ比較を省略する。
pub fn content_hashes_match(left: Option<&str>, right: Option<&str>) -> bool {
    let prefix = format!("{CONTENT_HASH_SCHEME}:");
    let valid = |value: &str| {
        value.strip_prefix(&prefix).is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
        })
    };
    matches!((left, right), (Some(a), Some(b)) if valid(a) && valid(b) && a == b)
}

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

/// carry-forward 複製（baseline 流用ショット）の決定的なスクリーンショット ID。
///
/// `(build_id, name)` から UUIDv5 で導出するため、比較ジョブのリトライ・再実行が
/// 何度走っても同じ ID・同じストレージキーに収束する。
///
/// - 前回の実行が「ストレージへ保存 → DB 挿入」の間で落ちても、再実行は同じ
///   キーへ上書き保存して行を挿入するので、孤児オブジェクトが自然に回収される
/// - 並行二重実行でも同じキーへの上書き PUT と `(build_id, name)` UNIQUE への
///   upsert に収束し、重複行・重複オブジェクトが生じない
pub fn carry_forward_screenshot_id(build_id: Uuid, name: &str) -> Uuid {
    // 固定文字列から導出した専用名前空間。通常アップロード（ランダム v4）と
    // 衝突しないよう、v5 の決定的空間に閉じ込める。
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"vrt:carry-forward");
    Uuid::new_v5(&namespace, format!("{build_id}/{name}").as_bytes())
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
    name: ScreenshotName,
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
///
/// 名前の規則（空白拒否・255 バイト上限）は [`ScreenshotName`] に一本化してあり、
/// この関数は検証済みの型しか受け取らない——計画・アップロード・finalize の
/// 突き合わせは名前の文字列一致なので、経路ごとに規則がずれると
/// 「計画には載るのに保存できない名前」ができ、そのビルドは finalize できなくなる。
#[allow(clippy::too_many_arguments)]
pub async fn store_screenshot_with_metadata<C: ConnectionTrait>(
    db: &C,
    storage: &Arc<dyn StorageBackend>,
    tenant_id: Uuid,
    project_id: Uuid,
    build_id: Uuid,
    name: ScreenshotName,
    bytes: Bytes,
    metadata: Option<serde_json::Value>,
) -> Result<screenshots::Model, AppError> {
    let name = name.into_string();
    let (width, height) = validate_png(&bytes)?;
    let content_hash = content_hash(&bytes);

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
        content_hash: Set(Some(content_hash)),
        metadata: Set(metadata),
        created_at: Set(Utc::now().fixed_offset()),
    }
    .insert(db)
    .await?)
}

/// CI からの直アップロード（`POST /v1/ci/builds/{id}/screenshots`）用の保存経路。
///
/// [`store_screenshot`] と違い、ビルド状態の検査（pending / screenshots モード /
/// capture plan の選択内 / 同名重複）を **build 行ロックの中で** DB 挿入と
/// 同時に行う。ハンドラの事前検査だけでは、計画添付（`attach_capture_plan`）と
/// 並行したアップロードが「添付前の検査を通った計画外ショット」として
/// 紛れ込みうる——添付側の「アップロード済みなら 409」という逆算防止も
/// 素通りする（直列化の規約は [`crate::review_lock`]）。
///
/// ストレージ保存は行ロックの**前**に行い、検査に落ちたら保存済みオブジェクトを
/// 補償削除する。ロックを保持したままストレージ IO を待つと、計画添付や
/// finalize まで巻き添えでブロックされるためである。キーは新規 UUID を含むので、
/// 先行保存が既存オブジェクトと衝突することはない。
pub async fn store_ci_screenshot(
    db: &DatabaseConnection,
    storage: &Arc<dyn StorageBackend>,
    tenant_id: Uuid,
    project_id: Uuid,
    build_id: Uuid,
    name: ScreenshotName,
    bytes: Bytes,
) -> Result<screenshots::Model, AppError> {
    let name = name.into_string();
    let (width, height) = validate_png(&bytes)?;
    let content_hash = content_hash(&bytes);

    let screenshot_id = Uuid::new_v4();
    let key = screenshot_key(tenant_id, project_id, build_id, screenshot_id);
    upload_png(storage, &key, bytes)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("upload screenshot: {e}")))?;

    let stored_key = key.clone();
    let inserted: Result<screenshots::Model, AppError> = with_transaction(db, move |txn| {
        Box::pin(async move {
            // ロック順 1（build のみ）。状態・計画はこの取り直した行を正とする。
            let build = crate::review_lock::build(txn, build_id).await?;
            if build.status != BuildStatus::Pending {
                return Err(AppError::Conflict);
            }
            if build.mode != BuildMode::Screenshots {
                return Err(AppError::Conflict);
            }
            // capture plan が固定されたビルドは、計画で選択された名前しか受け付けない。
            // 計画外の名前を黙って受けると、finalize の「計画 == アップロード」検証が
            // そこで初めて落ちるまで撮影のずれに気づけない。
            // 照合は HashSet（selected は最大 1 万件で、行ロック保持中の Vec 線形
            // 走査による文字列比較を避ける）。
            if let Some(plan) = crate::builds::capture_plan(&build)? {
                let selected: std::collections::HashSet<&str> =
                    plan.selected_names.iter().map(String::as_str).collect();
                if !selected.contains(name.as_str()) {
                    return Err(AppError::BadRequestDetail(format!(
                        "screenshot `{name}` is not in the capture plan attached to this build; \
                         only planned names can be uploaded (re-run the plan if the selection changed)"
                    )));
                }
            }

            let duplicate = screenshots::Entity::find()
                .filter(screenshots::Column::BuildId.eq(build_id))
                .filter(screenshots::Column::Name.eq(name.clone()))
                .one(txn)
                .await?;
            if duplicate.is_some() {
                return Err(AppError::Conflict);
            }

            Ok(screenshots::ActiveModel {
                id: Set(screenshot_id),
                build_id: Set(build_id),
                name: Set(name),
                storage_key: Set(stored_key),
                width: Set(width as i32),
                height: Set(height as i32),
                content_hash: Set(Some(content_hash)),
                metadata: Set(None),
                created_at: Set(Utc::now().fixed_offset()),
            }
            .insert(txn)
            .await?)
        })
    })
    .await;

    match inserted {
        Ok(model) => Ok(model),
        Err(err) => {
            // 検査に落ちた・挿入できなかったオブジェクトを補償削除する（ベストエフォート）。
            // 失敗しても行が無い以上どこからも参照されず、実害はゴミオブジェクトのみ。
            if let Err(delete_err) = storage.delete(&key).await {
                tracing::warn!(
                    %build_id,
                    key = %key,
                    error = %delete_err,
                    "failed to delete the storage object of a rejected screenshot upload"
                );
            }
            Err(err)
        }
    }
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
    fn identical_png_bytes_have_matching_content_hashes() {
        let bytes = png_bytes(2, 2);
        let hash = content_hash(&bytes);
        assert!(content_hashes_match(Some(&hash), Some(&hash)));
    }

    #[test]
    fn one_byte_difference_requires_normal_comparison() {
        let bytes = png_bytes(2, 2);
        let mut different = bytes.to_vec();
        let last = different.last_mut().expect("encoded PNG is non-empty");
        *last ^= 1;
        assert!(!content_hashes_match(
            Some(&content_hash(&bytes)),
            Some(&content_hash(&different)),
        ));
    }

    #[test]
    fn missing_or_unknown_hash_scheme_requires_normal_comparison() {
        let hash = content_hash(&png_bytes(2, 2));
        assert!(!content_hashes_match(None, Some(&hash)));
        assert!(!content_hashes_match(Some(&hash), None));
        assert!(!content_hashes_match(
            Some("sha999:abc"),
            Some("sha999:abc")
        ));
        assert!(!content_hashes_match(
            Some("sha256:abc"),
            Some("sha256:abc")
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
    fn carry_forward_id_is_deterministic_per_build_and_name() {
        let build_a = Uuid::new_v4();
        let build_b = Uuid::new_v4();

        // 同じ (build, name) は常に同じ ID（リトライが同じキーへ収束する根拠）。
        assert_eq!(
            carry_forward_screenshot_id(build_a, "home"),
            carry_forward_screenshot_id(build_a, "home"),
        );
        // build か name が違えば別 ID。
        assert_ne!(
            carry_forward_screenshot_id(build_a, "home"),
            carry_forward_screenshot_id(build_a, "about"),
        );
        assert_ne!(
            carry_forward_screenshot_id(build_a, "home"),
            carry_forward_screenshot_id(build_b, "home"),
        );
    }

    #[test]
    fn png_roundtrips_through_encode() {
        let image = RgbaImage::from_pixel(3, 4, Rgba([9, 8, 7, 255]));
        let bytes = encode_png(&image).expect("encode");
        assert_eq!(validate_png(&bytes).expect("valid"), (3, 4));
    }
}
