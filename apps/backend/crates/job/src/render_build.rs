//! アップロードされた Storybook バンドルをヘッドレス Chromium で撮影するジョブ。
//!
//! `POST /v1/ci/builds/{id}/finalize`（`mode = storybook`）が
//! `pending → rendering` の遷移と同時に投入する。
//!
//! 処理の流れ:
//!
//! 1. build / project をロードし、`rendering` でなければ何もせず終わる（重複投入の保護）
//! 2. `builds.storybook_key` の zip をストレージから読み、一時ディレクトリへ安全に展開
//!    （[`service::render::bundle`] が zip-slip / zip bomb / symlink を弾く）
//! 3. `index.json` からストーリー一覧を作り、ループバックの静的サーバーを立てる
//! 4. ストーリーを**逐次**レンダリングして PNG を `screenshots` に保存
//!    （name は `{title}/{name}`、metadata に `{story_id, title}`）
//! 5. `rendering → processing` に遷移し、`CompareBuildJob` を投入して既存の比較経路へ繋ぐ
//!
//! リトライ安全性: 開始時にそのビルドのスクリーンショット行を全削除するため、
//! 途中で落ちて再実行されても `(build_id, name)` の UNIQUE 制約にぶつからない。

use apalis::prelude::{BoxDynError, Data, TaskSink};
use apalis_postgres::{Config, PgPool, PostgresStorage};
use sea_orm::prelude::Uuid;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use entity::{builds::BuildMode, builds::BuildStatus, screenshots};
use service::render::{RenderOptions, StaticServer, StoryRenderer};

use crate::JobState;

pub const QUEUE_NAME: &str = "render_build";
pub const MAX_RETRIES: usize = 2;
/// ワーカーの同時実行数。1 ジョブがブラウザ 1 個を丸ごと持つので控えめにする。
pub const WORKER_CONCURRENCY: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderBuildJob {
    pub build_id: Uuid,
}

/// `RenderBuildJob` のストレージ。
///
/// フェッチャの選択理由は [`crate::compare_build::CompareBuildStorage`] と同じ
/// （通知型は起動前に投入されたジョブを取りこぼすためポーリング型を使う）。
pub type RenderBuildStorage = PostgresStorage<RenderBuildJob>;

pub fn build_storage_for_queue(pool: &PgPool, queue: &str) -> RenderBuildStorage {
    PostgresStorage::new_with_config(pool, &Config::new(queue))
}

pub fn build_storage(pool: &PgPool) -> RenderBuildStorage {
    build_storage_for_queue(pool, QUEUE_NAME)
}

pub async fn setup(pool: &PgPool) -> Result<Arc<RenderBuildStorage>, anyhow::Error> {
    setup_with_queue(pool, QUEUE_NAME).await
}

/// キュー名を指定してセットアップする（統合テスト用）。
pub async fn setup_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<RenderBuildStorage>, anyhow::Error> {
    crate::ensure_apalis_schema(pool).await?;
    Ok(Arc::new(build_storage_for_queue(pool, queue)))
}

pub async fn enqueue(
    storage: &RenderBuildStorage,
    job: RenderBuildJob,
) -> Result<(), anyhow::Error> {
    let mut storage = storage.clone();
    storage
        .push(job)
        .await
        .map_err(|e| anyhow::anyhow!("push render build job: {e}"))?;
    Ok(())
}

/// ワーカーのエントリポイント。
///
/// 回復不能なエラーはビルドを `failed` に落として `Ok(())` を返す（無限リトライ回避）。
/// `Err` を返すのはビルド行にすら書き戻せなかったケースだけ。
pub async fn process(job: RenderBuildJob, state: Data<JobState>) -> Result<(), BoxDynError> {
    let build_id = job.build_id;

    match run(build_id, &state).await {
        Ok(()) => Ok(()),
        Err(err) => {
            tracing::error!(%build_id, error = %err, "render build job failed");
            let build = service::builds::get_build(&state.db, build_id)
                .await
                .map_err(|e| -> BoxDynError { format!("reload build {build_id}: {e}").into() })?;
            service::builds::mark_failed(&state.db, build, truncate(&err.to_string(), 2000))
                .await
                .map_err(|e| -> BoxDynError {
                    format!("mark build {build_id} failed: {e}").into()
                })?;

            // レンダリング失敗もビルドの終端なので GitHub にステータスを返す。
            crate::github_status::enqueue_best_effort(&state.github_status_storage, build_id).await;
            Ok(())
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

async fn run(build_id: Uuid, state: &JobState) -> Result<(), anyhow::Error> {
    let db = &state.db;

    let build = service::builds::get_build(db, build_id).await?;
    if build.status != BuildStatus::Rendering {
        tracing::info!(%build_id, status = ?build.status, "skipping render job for non-rendering build");
        return Ok(());
    }
    if build.mode != BuildMode::Storybook {
        anyhow::bail!("build {build_id} is not a storybook-mode build");
    }

    let storybook_key = build
        .storybook_key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("build {build_id} has no storybook bundle"))?;

    let project = service::projects::get_project(db, build.project_id).await?;

    let chromium_path = state
        .settings
        .chromium_path
        .clone()
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("storybook rendering is not configured (CHROMIUM_PATH is unset)")
        })?;

    // リトライ安全性: 前回の途中結果を捨ててからやり直す。
    // （`(build_id, name)` の UNIQUE にぶつかると 2 回目以降が必ず落ちる。）
    screenshots::Entity::delete_many()
        .filter(screenshots::Column::BuildId.eq(build_id))
        .exec(db)
        .await?;

    let bytes = service::render::download_bundle(&state.storage, &storybook_key).await?;

    // 一時ディレクトリは TempDir が drop されるときに必ず消える
    // （成功・失敗・panic のいずれでも）。
    let workdir = tempfile::Builder::new()
        .prefix("vrt-storybook-")
        .tempdir()
        .map_err(|e| anyhow::anyhow!("create temp dir: {e}"))?;

    let bundle = {
        let dest = workdir.path().to_path_buf();
        // 展開は同期 IO + 解凍で CPU バウンド。ワーカーのランタイムを塞がない。
        tokio::task::spawn_blocking(move || service::render::extract_and_index(&bytes, &dest))
            .await
            .map_err(|e| anyhow::anyhow!("bundle extraction task join: {e}"))??
    };

    if bundle.stories.is_empty() {
        anyhow::bail!("storybook bundle contains no stories (only docs entries?)");
    }

    tracing::info!(
        %build_id,
        stories = bundle.stories.len(),
        "rendering storybook bundle"
    );

    let server = StaticServer::start(&bundle.root).await?;
    let base_url = server.base_url();

    let options = RenderOptions::new(
        chromium_path,
        project.viewport_width.max(1) as u32,
        project.viewport_height.max(1) as u32,
    );
    let renderer = StoryRenderer::launch(options).await?;

    // ブラウザは成功・失敗どちらでも必ず閉じる（`?` で早期 return しない）。
    let outcome = render_all(state, &project, &build, &renderer, &base_url, &bundle).await;
    renderer.close().await;
    drop(server);
    outcome?;

    let build = service::builds::get_build(db, build_id).await?;
    let build = service::builds::transition(db, build, BuildStatus::Processing).await?;

    // レンダリングが済んだので既存の比較パイプラインへ引き渡す。
    // `github_status` を compare_build が投入するのと同じチェーンパターン。
    crate::compare_build::enqueue(
        &state.compare_build_storage,
        crate::CompareBuildJob { build_id },
    )
    .await?;

    tracing::info!(%build_id, number = build.number, "storybook render finished; compare job enqueued");

    Ok(())
}

/// 全ストーリーを逐次レンダリングして保存する。
///
/// MVP では 1 件でも失敗したらビルドごと失敗にする。
/// （代替案: 失敗したストーリーだけ `comparisons` に error 行を作って残りは通す。
/// レビュー UI に「撮れなかった」を出す設計が必要なので、そこは後続で扱う。）
async fn render_all(
    state: &JobState,
    project: &entity::projects::Model,
    build: &entity::builds::Model,
    renderer: &StoryRenderer,
    base_url: &str,
    bundle: &service::render::ExtractedBundle,
) -> Result<(), anyhow::Error> {
    for story in &bundle.stories {
        let png = renderer
            .render_story(base_url, &story.id)
            .await
            .map_err(|e| anyhow::anyhow!("render story `{}`: {e}", story.id))?;

        let metadata = serde_json::json!({
            "story_id": story.id,
            "title": story.title,
        });

        service::screenshots::store_screenshot_with_metadata(
            &state.db,
            &state.storage,
            project.tenant_id,
            project.id,
            build.id,
            story.screenshot_name(),
            bytes::Bytes::from(png),
            Some(metadata),
        )
        .await
        .map_err(|e| anyhow::anyhow!("store screenshot for story `{}`: {e}", story.id))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_limits_error_messages() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate(&"x".repeat(50), 10).len(), 10);
    }

    #[test]
    fn queue_name_is_stable() {
        // ワーカー名は `{queue}-worker-{uuid}` で組み立てられる（server.rs 参照）。
        assert_eq!(QUEUE_NAME, "render_build");
    }
}
