//! ビルド関連の DTO。

use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

use entity::{builds, builds::BuildMode, builds::BuildStatus, screenshots};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub project_id: Uuid,
    /// プロジェクト内で連番のビルド番号。
    pub number: i64,
    pub branch: String,
    pub commit_sha: String,
    #[schema(nullable)]
    pub commit_message: Option<String>,
    #[schema(nullable)]
    pub pull_request_number: Option<i32>,
    pub status: BuildStatus,
    /// 入力形式（`screenshots` = CI がアップロード / `storybook` = サーバーがレンダリング）。
    pub mode: BuildMode,
    /// storybook モードでバンドルがアップロード済みか。
    pub storybook_uploaded: bool,
    /// 比較に使った baseline（未確定なら null）。
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub baseline_id: Option<Uuid>,
    pub total_count: i32,
    pub changed_count: i32,
    pub added_count: i32,
    pub removed_count: i32,
    pub unchanged_count: i32,
    #[schema(nullable)]
    pub error_message: Option<String>,
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub approved_by: Option<Uuid>,
    #[schema(value_type = Option<String>, format = "date-time", nullable)]
    pub approved_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = "date-time", nullable)]
    pub completed_at: Option<DateTime<Utc>>,
}

impl From<builds::Model> for BuildResponse {
    fn from(model: builds::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            number: model.number,
            branch: model.branch,
            commit_sha: model.commit_sha,
            commit_message: model.commit_message,
            pull_request_number: model.pull_request_number,
            status: model.status,
            mode: model.mode,
            // ストレージキー自体は内部情報なので露出させず、有無だけ返す。
            storybook_uploaded: model.storybook_key.is_some(),
            baseline_id: model.baseline_id,
            total_count: model.total_count,
            changed_count: model.changed_count,
            added_count: model.added_count,
            removed_count: model.removed_count,
            unchanged_count: model.unchanged_count,
            error_message: model.error_message,
            approved_by: model.approved_by,
            approved_at: model.approved_at.map(|t| t.with_timezone(&Utc)),
            created_at: model.created_at.with_timezone(&Utc),
            completed_at: model.completed_at.map(|t| t.with_timezone(&Utc)),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildListResponse {
    pub builds: Vec<BuildResponse>,
    pub total: u64,
}

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct CreateBuildRequest {
    /// 対象ブランチ。baseline の解決キーになる。
    #[validate(length(min = 1, max = 255))]
    pub branch: String,
    #[validate(length(min = 1, max = 100))]
    pub commit_sha: String,
    #[validate(length(max = 4000))]
    pub commit_message: Option<String>,
    /// PR 番号（Phase 6 の GitHub ステータス連携で使う）。
    pub pull_request_number: Option<i32>,
    /// 入力形式。省略時は `screenshots`（従来どおり CI が PNG をアップロードする）。
    /// `storybook` を指定すると `POST /v1/ci/builds/{id}/storybook` でバンドルを送る形になる。
    #[serde(default)]
    pub mode: Option<BuildMode>,
}

/// Storybook バンドルのアップロード結果。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StorybookBundleResponse {
    #[schema(value_type = String, format = "uuid")]
    pub build_id: Uuid,
    /// 受け取った zip のバイト数。
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScreenshotResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub build_id: Uuid,
    pub name: String,
    pub width: i32,
    pub height: i32,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
}

impl From<screenshots::Model> for ScreenshotResponse {
    fn from(model: screenshots::Model) -> Self {
        Self {
            id: model.id,
            build_id: model.build_id,
            name: model.name,
            width: model.width,
            height: model.height,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}

/// ビルド承認リクエスト。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ApproveBuildRequest {
    /// `true` にすると未レビューの比較もまとめて承認する（一括承認）。
    #[serde(default)]
    pub force: bool,
}

/// ビルド一覧のページネーションパラメータ。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BuildListQuery {
    /// 取得件数（1〜100、既定 30）。
    pub limit: Option<u64>,
    /// スキップ件数（既定 0）。
    pub offset: Option<u64>,
}
