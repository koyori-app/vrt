//! GitHub Webhook のイベント処理ジョブ（task のキュー投入パターンを踏襲）。
//!
//! ハンドラ（`POST /v1/github/webhook`）は署名検証だけをその場で行い、
//! 本文の解釈はこのジョブに委ねる。webhook の応答が遅いと GitHub 側で
//! 配信失敗扱いになるため、DB 書き込みは非同期に逃がす。
//!
//! 扱うのは `installation` イベントだけ:
//!
//! | action | 処理 |
//! |---|---|
//! | `created` | `github_installations` に upsert（既存行があれば復活させる） |
//! | `deleted` | `deleted_at` を打ち、`tenant_id` を外し、紐付いたプロジェクトを解除 |
//! | `suspend` | `suspended_at` を打つ |
//! | `unsuspend` | `suspended_at` を消す |
//!
//! それ以外のイベント / action は debug ログのみで `Ok`（GitHub は購読していない
//! イベントも送ってくることがある）。

use std::sync::Arc;

use apalis::prelude::{BoxDynError, Data, TaskSink};
use apalis_postgres::{Config, PgPool, PostgresStorage};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    prelude::Uuid,
};
use serde::{Deserialize, Serialize};

use entity::{github_installations, projects};

use crate::JobState;

pub const QUEUE_NAME: &str = "github_webhook";
pub const MAX_RETRIES: usize = 5;
pub const WORKER_CONCURRENCY: usize = 4;

/// webhook の生イベント。解釈はワーカー側で行う。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubWebhookJob {
    /// `X-GitHub-Event` の値。
    pub event: String,
    /// `X-GitHub-Delivery` の値（ログの相関用）。
    pub delivery_id: Option<String>,
    /// リクエストボディ（JSON）。
    pub payload: serde_json::Value,
}

pub type GithubWebhookStorage = PostgresStorage<GithubWebhookJob>;

pub fn build_storage_for_queue(pool: &PgPool, queue: &str) -> GithubWebhookStorage {
    PostgresStorage::new_with_config(pool, &Config::new(queue))
}

pub fn build_storage(pool: &PgPool) -> GithubWebhookStorage {
    build_storage_for_queue(pool, QUEUE_NAME)
}

pub async fn setup(pool: &PgPool) -> Result<Arc<GithubWebhookStorage>, anyhow::Error> {
    setup_with_queue(pool, QUEUE_NAME).await
}

/// キュー名を指定してセットアップする（統合テスト用）。
pub async fn setup_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<GithubWebhookStorage>, anyhow::Error> {
    // apalis のジョブテーブル作成はキューごとではなく DB 全体の操作なので、
    // プロセス内で 1 回に絞る（[`crate::ensure_apalis_schema`] 参照）。
    crate::ensure_apalis_schema(pool).await?;
    Ok(Arc::new(build_storage_for_queue(pool, queue)))
}

pub async fn enqueue(
    storage: &GithubWebhookStorage,
    job: GithubWebhookJob,
) -> Result<(), anyhow::Error> {
    let mut storage = storage.clone();
    storage
        .push(job)
        .await
        .map_err(|e| anyhow::anyhow!("push github webhook job: {e}"))?;
    Ok(())
}

pub async fn process(job: GithubWebhookJob, state: Data<JobState>) -> Result<(), BoxDynError> {
    let delivery_id = job.delivery_id.clone();
    match run(&job, &state).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!(
                event = %job.event,
                delivery_id = ?delivery_id,
                error = %e,
                "github webhook job failed"
            );
            Err(e.into())
        }
    }
}

async fn run(job: &GithubWebhookJob, state: &JobState) -> Result<(), anyhow::Error> {
    if job.event != "installation" {
        tracing::debug!(
            event = %job.event,
            delivery_id = ?job.delivery_id,
            "ignoring github webhook event"
        );
        return Ok(());
    }

    let action = job
        .payload
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let Some(installation) = job.payload.get("installation") else {
        tracing::warn!(
            delivery_id = ?job.delivery_id,
            "installation event without installation object"
        );
        return Ok(());
    };
    let Some(installation_id) = installation.get("id").and_then(serde_json::Value::as_i64) else {
        tracing::warn!(delivery_id = ?job.delivery_id, "installation event without id");
        return Ok(());
    };

    match action {
        "created" => {
            let account = installation.get("account");
            let account_login = account
                .and_then(|a| a.get("login"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let account_type = account
                .and_then(|a| a.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(github_installations::DEFAULT_ACCOUNT_TYPE)
                .to_string();
            upsert_installation(&state.db, installation_id, account_login, account_type).await?;
        }
        "deleted" => delete_installation(&state.db, installation_id).await?,
        "suspend" => set_suspended(&state.db, installation_id, true).await?,
        "unsuspend" => set_suspended(&state.db, installation_id, false).await?,
        other => {
            tracing::debug!(
                action = %other,
                installation_id,
                "ignoring installation action"
            );
        }
    }

    Ok(())
}

/// `installation.created`。同じ installation_id の行があれば作り直さず復活させる
/// （再インストール時に既存の claim を維持するため `tenant_id` は触らない）。
async fn upsert_installation<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
    account_login: String,
    account_type: String,
) -> Result<(), anyhow::Error> {
    let now = Utc::now().fixed_offset();
    let existing = github_installations::Entity::find()
        .filter(github_installations::Column::InstallationId.eq(installation_id))
        .one(db)
        .await?;

    match existing {
        Some(model) => {
            let mut active: github_installations::ActiveModel = model.into();
            active.account_login = Set(account_login);
            active.account_type = Set(account_type);
            active.suspended_at = Set(None);
            active.deleted_at = Set(None);
            active.updated_at = Set(now);
            active.update(db).await?;
        }
        None => {
            github_installations::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(None),
                installation_id: Set(installation_id),
                account_login: Set(account_login),
                account_type: Set(account_type),
                suspended_at: Set(None),
                deleted_at: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(db)
            .await?;
        }
    }

    tracing::info!(installation_id, "github installation created");
    Ok(())
}

/// `installation.deleted`。行は監査のため残し、claim とプロジェクトの紐付けだけ外す。
async fn delete_installation<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
) -> Result<(), anyhow::Error> {
    let now = Utc::now().fixed_offset();

    let Some(model) = github_installations::Entity::find()
        .filter(github_installations::Column::InstallationId.eq(installation_id))
        .one(db)
        .await?
    else {
        tracing::debug!(
            installation_id,
            "installation.deleted for unknown installation"
        );
        return Ok(());
    };

    let mut active: github_installations::ActiveModel = model.into();
    active.tenant_id = Set(None);
    active.deleted_at = Set(Some(now));
    active.updated_at = Set(now);
    active.update(db).await?;

    // アンインストール後にステータスを投げても 401 になるだけなので、
    // プロジェクト側の紐付けも同時に外す。
    let unlinked = projects::Entity::update_many()
        .col_expr(
            projects::Column::GithubInstallationId,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            projects::Column::GithubRepo,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            projects::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(projects::Column::GithubInstallationId.eq(installation_id))
        .exec(db)
        .await?;

    tracing::info!(
        installation_id,
        unlinked_projects = unlinked.rows_affected,
        "github installation deleted"
    );
    Ok(())
}

/// `installation.suspend` / `unsuspend`。
async fn set_suspended<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
    suspended: bool,
) -> Result<(), anyhow::Error> {
    let now = Utc::now().fixed_offset();

    let Some(model) = github_installations::Entity::find()
        .filter(github_installations::Column::InstallationId.eq(installation_id))
        .one(db)
        .await?
    else {
        tracing::debug!(installation_id, "suspend event for unknown installation");
        return Ok(());
    };

    let mut active: github_installations::ActiveModel = model.into();
    active.suspended_at = Set(suspended.then_some(now));
    active.updated_at = Set(now);
    active.update(db).await?;

    tracing::info!(
        installation_id,
        suspended,
        "github installation suspension changed"
    );
    Ok(())
}
