//! ビルドの状態を GitHub のコミットステータスと PR コメントへ反映するジョブ。
//!
//! コミットステータスに加えて、ビルドが PR に紐付いている
//! （`pull_request_number` がある）場合はレビュー UI へのリンクを PR コメントとして
//! 掲示する（マーカー付きコメントの upsert。Chromatic と同じ見せ方）。
//!
//! 投入元は 3 箇所:
//!
//! - `POST /v1/ci/builds/{id}/finalize`（`processing` → pending ステータス）
//! - [`crate::compare_build`] の完了時（`passed` / `changes_detected` / `failed`）
//! - `POST /v1/builds/{id}/approve|reject`（レビュー結果）
//!
//! ジョブ側では「投げてよいか」を毎回確認する:
//! プロジェクトが installation + リポジトリに紐付いていない、あるいは GitHub App が
//! 未設定なら、何もせず `Ok` を返す（ログのみ）。そのため投入側は条件を気にせず投げてよい。
//!
//! リトライ方針: ネットワーク断・5xx は `Err` を返して apalis のリトライに委ねる。
//! 4xx（リポジトリが無い・権限が無い等）はリトライしても直らないので警告ログ + `Ok`。

use std::sync::Arc;

use apalis::prelude::{BoxDynError, Data, TaskSink};
use apalis_postgres::{Config, PgPool, PostgresStorage};
use sea_orm::{EntityTrait, prelude::Uuid};
use serde::{Deserialize, Serialize};

use entity::{builds, projects, tenants};
use service::github::{
    CommentWrite, GithubApiError, STATUS_CONTEXT, build_target_url, github_app, installation_token,
    latest_status_build_number, post_commit_status, pr_comment_body, pr_comment_marker,
    status_for_build, upsert_pr_comment,
};

use crate::JobState;

pub const QUEUE_NAME: &str = "github_status";
pub const MAX_RETRIES: usize = 5;
/// ステータス POST は I/O バウンドなので compare_build より多めでよい。
pub const WORKER_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubStatusJob {
    pub build_id: Uuid,
}

/// `compare_build` と同じ理由でポーリング型のフェッチャを使う
/// （`PgNotify` はワーカー起動前に投入された通知を取りこぼす）。
pub type GithubStatusStorage = PostgresStorage<GithubStatusJob>;

pub fn build_storage_for_queue(pool: &PgPool, queue: &str) -> GithubStatusStorage {
    PostgresStorage::new_with_config(pool, &Config::new(queue))
}

pub fn build_storage(pool: &PgPool) -> GithubStatusStorage {
    build_storage_for_queue(pool, QUEUE_NAME)
}

pub async fn setup(pool: &PgPool) -> Result<Arc<GithubStatusStorage>, anyhow::Error> {
    setup_with_queue(pool, QUEUE_NAME).await
}

/// キュー名を指定してセットアップする（統合テスト用）。
pub async fn setup_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<GithubStatusStorage>, anyhow::Error> {
    // apalis のジョブテーブル作成はキューごとではなく DB 全体の操作なので、
    // プロセス内で 1 回に絞る（[`crate::ensure_apalis_schema`] 参照）。
    crate::ensure_apalis_schema(pool).await?;
    Ok(Arc::new(build_storage_for_queue(pool, queue)))
}

pub async fn enqueue(
    storage: &GithubStatusStorage,
    job: GithubStatusJob,
) -> Result<(), anyhow::Error> {
    let mut storage = storage.clone();
    storage
        .push(job)
        .await
        .map_err(|e| anyhow::anyhow!("push github status job: {e}"))?;
    Ok(())
}

/// 投入に失敗してもリクエスト本体は失敗させない、ベストエフォートの投入。
///
/// コミットステータスは補助的な表示でしかないため、これが落ちてもビルドの
/// finalize / 承認そのものは成立させる。
pub async fn enqueue_best_effort(storage: &GithubStatusStorage, build_id: Uuid) {
    if let Err(e) = enqueue(storage, GithubStatusJob { build_id }).await {
        tracing::warn!(%build_id, error = %e, "failed to enqueue github status job");
    }
}

pub async fn process(job: GithubStatusJob, state: Data<JobState>) -> Result<(), BoxDynError> {
    let build_id = job.build_id;
    match run(build_id, &state).await {
        Ok(()) => Ok(()),
        // 4xx は何度投げても同じ。ジョブは成功扱いにして打ち切る。
        Err(GithubApiError::Permanent(message)) => {
            tracing::warn!(%build_id, %message, "github status job gave up (permanent error)");
            Ok(())
        }
        Err(GithubApiError::Transient(error)) => {
            tracing::warn!(%build_id, %error, "github status job failed, will retry");
            Err(error.into())
        }
    }
}

fn transient(context: &str, error: impl std::fmt::Display) -> GithubApiError {
    GithubApiError::Transient(anyhow::anyhow!("{context}: {error}"))
}

async fn run(build_id: Uuid, state: &JobState) -> Result<(), GithubApiError> {
    let db = &state.db;

    let Some(build) = builds::Entity::find_by_id(build_id)
        .one(db)
        .await
        .map_err(|e| transient("load build", e))?
    else {
        tracing::debug!(%build_id, "github status job: build no longer exists");
        return Ok(());
    };

    let Some(project) = projects::Entity::find_by_id(build.project_id)
        .one(db)
        .await
        .map_err(|e| transient("load project", e))?
    else {
        tracing::debug!(%build_id, "github status job: project no longer exists");
        return Ok(());
    };

    let (Some(installation_id), Some(repo)) = (
        project.github_installation_id,
        project.github_repo.as_deref(),
    ) else {
        tracing::debug!(
            %build_id,
            project_id = %project.id,
            "github status job: project is not linked to a github repository"
        );
        return Ok(());
    };

    let Some(app) = github_app(&state.settings, &state.http) else {
        tracing::warn!(
            %build_id,
            "github status job: github app is not configured (GITHUB_APP_ID / GITHUB_APP_PRIVATE_KEY_PEM)"
        );
        return Ok(());
    };

    let Some(tenant) = tenants::Entity::find_by_id(project.tenant_id)
        .one(db)
        .await
        .map_err(|e| transient("load tenant", e))?
    else {
        tracing::debug!(%build_id, "github status job: tenant no longer exists");
        return Ok(());
    };

    let token = installation_token(&state.redis_client, &app, installation_id).await?;
    let (commit_state, description) = status_for_build(&build);
    let target_url = build_target_url(
        &state.settings.app_url,
        &tenant.slug,
        &project.slug,
        build.number,
    );

    // commit status は required status としてマージの門を守るので、PR コメントと同じく
    // 遅延した古いビルドが新しい結果を巻き戻さないようにする。既存ステータスの
    // target_url からビルド番号を読む（commit status にはメタデータを埋められない）。
    let existing_status_number = latest_status_build_number(
        &state.http,
        &state.settings.github_api_base_url(),
        &token,
        repo,
        &build.commit_sha,
        STATUS_CONTEXT,
    )
    .await?;

    if let Some(existing_build_number) =
        existing_status_number.filter(|number| *number > build.number)
    {
        tracing::info!(
            %build_id,
            repo,
            sha = %build.commit_sha,
            build_number = build.number,
            existing_build_number,
            "skipped stale github commit status"
        );
    } else {
        post_commit_status(
            &state.http,
            &state.settings.github_api_base_url(),
            &token,
            repo,
            &build.commit_sha,
            commit_state,
            &description,
            Some(&target_url),
            STATUS_CONTEXT,
        )
        .await?;

        tracing::info!(
            %build_id,
            repo,
            sha = %build.commit_sha,
            state = %commit_state,
            "posted github commit status"
        );
    }

    // PR に紐付くビルドはレビュー UI へのリンクをコメントとして掲示する。
    // ステータスの後に置く: コメントが transient に失敗してリトライされても、
    // ステータスの再 POST は無害（GitHub は context ごとに最新だけを表示する）。
    if let Some(pr_number) = build.pull_request_number {
        let marker = pr_comment_marker(project.id);
        let body = pr_comment_body(
            &marker,
            &project.slug,
            build.number,
            &description,
            &target_url,
        );
        let outcome = upsert_pr_comment(
            &state.http,
            &state.settings.github_api_base_url(),
            &token,
            repo,
            pr_number,
            &marker,
            build.number,
            &body,
        )
        .await?;

        match outcome {
            CommentWrite::Wrote => {
                tracing::info!(%build_id, repo, pr_number, "upserted github pr comment");
            }
            CommentWrite::SkippedStale {
                existing_build_number,
            } => {
                tracing::info!(
                    %build_id,
                    repo,
                    pr_number,
                    build_number = build.number,
                    existing_build_number,
                    "skipped stale github pr comment update"
                );
            }
        }
    }

    Ok(())
}
