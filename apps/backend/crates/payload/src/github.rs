//! GitHub App 連携の DTO。

use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use entity::github_installations;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GithubAppResponse {
    /// App の資格情報が設定済みか。
    pub enabled: bool,
    /// GitHub のインストール画面。未設定なら `null`。
    #[schema(nullable, example = "https://github.com/apps/vrt/installations/new")]
    pub install_url: Option<String>,
    /// GitHub App に設定すべき setup URL。
    #[schema(example = "https://vrt.example.com/github/setup")]
    pub setup_url: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GithubInstallationResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    /// claim 済みのテナント。未 claim なら `null`。
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub tenant_id: Option<Uuid>,
    /// GitHub 側の installation ID（プロジェクト紐付けで使う）。
    pub installation_id: i64,
    pub account_login: String,
    /// `User` | `Organization`。
    pub account_type: String,
    /// GitHub 側で suspend されているか。
    pub suspended: bool,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
}

impl From<github_installations::Model> for GithubInstallationResponse {
    fn from(model: github_installations::Model) -> Self {
        Self {
            id: model.id,
            tenant_id: model.tenant_id,
            installation_id: model.installation_id,
            account_login: model.account_login,
            account_type: model.account_type,
            suspended: model.suspended_at.is_some(),
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GithubInstallationListResponse {
    pub installations: Vec<GithubInstallationResponse>,
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct GithubRepositoryResponse {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub archived: bool,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GithubRepositoryListResponse {
    pub repositories: Vec<GithubRepositoryResponse>,
    pub total: u64,
}

/// `GET /v1/github/installations` のクエリ。
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct InstallationListQuery {
    /// 対象テナント。メンバーであることが必要。
    #[param(value_type = String, format = "uuid")]
    pub tenant_id: Uuid,
}

/// `POST /v1/github/installations/{installation_id}/claim` のボディ。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ClaimInstallationRequest {
    /// この installation を紐付けるテナント。admin 以上であることが必要。
    #[schema(value_type = String, format = "uuid")]
    pub tenant_id: Uuid,
    /// `POST /v1/github/setup/state` で発行した one-time state。
    /// 発行したユーザー・テナントと一致し、未使用かつ期限内である必要がある。
    pub state: String,
}

/// `POST /v1/github/setup/state` のボディ。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSetupStateRequest {
    /// インストール先として想定するテナント。admin 以上であることが必要。
    #[schema(value_type = String, format = "uuid")]
    pub tenant_id: Uuid,
}

/// `POST /v1/github/setup/state` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateSetupStateResponse {
    /// GitHub のインストール URL に `state` として付与する値。
    pub state: String,
    /// state の有効期限（秒）。
    pub expires_in: u64,
}

/// `PATCH /v1/projects/{project_id}/github` のボディ。
///
/// 2 つのフィールドは常にセットで扱う。両方 `null` なら連携解除。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProjectGithubRequest {
    /// 紐付ける installation の GitHub ID。解除するときは `null`。
    #[serde(default)]
    #[schema(nullable)]
    pub installation_id: Option<i64>,
    /// `owner/name` 形式。解除するときは `null`。
    #[serde(default)]
    #[schema(nullable, example = "octocat/hello-world")]
    pub github_repo: Option<String>,
}
