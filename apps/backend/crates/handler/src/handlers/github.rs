//! GitHub App 連携の HTTP ハンドラ。
//!
//! - `POST /v1/github/webhook` — **公開エンドポイント**（セッションも PAT も要らない）。
//!   `X-Hub-Signature-256` の HMAC-SHA256 を定数時間比較で検証し、通ったら
//!   [`job::github_webhook::GithubWebhookJob`] を投入して 202 を返す。
//!   本文の解釈はワーカー側。CSRF ミドルウェアは Origin ヘッダが無いリクエストを
//!   素通しするため、GitHub からの配信はそのまま通る。
//! - `GET /v1/github/installations` — テナントが claim 済みの installation 一覧。
//! - `GET /v1/github/installations/unclaimed` — 未 claim の installation 一覧。
//! - `POST /v1/github/installations/{installation_id}/claim` — テナントへの紐付け。
//! - `PATCH /v1/projects/{project_id}/github` — プロジェクトとリポジトリの紐付け。

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::prelude::Uuid;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::AppState;
use crate::error::{AppError, ServerError};
use crate::extractors::AuthUser;
use crate::openapi::CrudErrors;
use entity::tenant_members::TenantRole;
use payload::github::*;
use payload::projects::ProjectResponse;
use service::github as github_service;
use service::tenants as tenant_service;

type HmacSha256 = Hmac<Sha256>;

/// webhook ボディの上限。GitHub の配信は通常数十 KB で、1MB あれば十分に余裕がある。
pub const MAX_WEBHOOK_BODY_BYTES: usize = 1024 * 1024;

/// `X-Hub-Signature-256`（`sha256=<hex>`）を検証する。
///
/// 比較は [`ConstantTimeEq`] で行い、先頭一致からシークレットを推定されないようにする。
pub fn verify_webhook_signature(secret: &str, signature_header: &str, body: &[u8]) -> bool {
    let Some(hex_digest) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_digest) else {
        return false;
    };
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(body);
    let computed = mac.finalize().into_bytes();
    // 長さが違う場合 ct_eq は false を返す（ここで早期 return しても情報は漏れない）。
    expected.ct_eq(computed.as_slice()).into()
}

// ── /v1/github/webhook ──────────────────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/webhook",
    tag = "GitHub",
    summary = "GitHub Webhook 受信",
    description = "GitHub App からの配信を受け取る公開エンドポイント。\
                   `X-Hub-Signature-256` を検証し、ジョブに積んで即座に 202 を返す。",
    responses(
        (status = 202, description = "受理してジョブに積みました"),
        (status = 400, description = "GitHub App（webhook secret）が未設定、または本文が JSON でない", body = ServerError),
        (status = 401, description = "署名がありません / 一致しません", body = ServerError),
    )
)]
pub async fn github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let Some(secret) = state.settings.github_webhook_secret.as_deref() else {
        return Err(AppError::BadRequestDetail(
            "github app not configured".into(),
        ));
    };

    let signature = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    if !verify_webhook_signature(secret, signature, &body) {
        tracing::warn!("github webhook signature mismatch");
        return Err(AppError::Unauthorized);
    }

    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let delivery_id = headers
        .get("X-GitHub-Delivery")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequestDetail(format!("invalid webhook body: {e}")))?;

    job::github_webhook::enqueue(
        &state.github_webhook_storage,
        job::GithubWebhookJob {
            event,
            delivery_id,
            payload,
        },
    )
    .await
    .map_err(AppError::Internal)?;

    Ok(StatusCode::ACCEPTED)
}

// ── /v1/github/installations ────────────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/installations",
    tag = "GitHub",
    summary = "テナントの GitHub installation 一覧",
    description = "対象テナントのメンバーであること。アンインストール済みの installation は含まない。",
    params(InstallationListQuery),
    responses(
        (status = 200, description = "installation 一覧", body = GithubInstallationListResponse),
        CrudErrors,
    )
)]
pub async fn list_installations(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<InstallationListQuery>,
) -> Result<Json<GithubInstallationListResponse>, AppError> {
    auth.require_session()?;
    tenant_service::require_role(&state.db, query.tenant_id, auth.user_id, TenantRole::Member)
        .await?;

    let list = github_service::list_installations_for_tenant(&state.db, query.tenant_id).await?;
    Ok(Json(GithubInstallationListResponse {
        total: list.len() as u64,
        installations: list.into_iter().map(Into::into).collect(),
    }))
}

#[axum::debug_handler]
#[utoipa::path(
    get,
    path = "/installations/unclaimed",
    tag = "GitHub",
    summary = "未 claim の GitHub installation 一覧",
    description = "GitHub App をインストールした直後の導線用。ログイン済みなら誰でも参照できる \
                   （MVP の割り切り。見えるのはアカウント名と種別のみで、claim は先着 1 テナント）。",
    responses(
        (status = 200, description = "未 claim の installation 一覧", body = GithubInstallationListResponse),
        CrudErrors,
    )
)]
pub async fn list_unclaimed_installations(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<GithubInstallationListResponse>, AppError> {
    auth.require_session()?;
    let list = github_service::list_unclaimed_installations(&state.db).await?;
    Ok(Json(GithubInstallationListResponse {
        total: list.len() as u64,
        installations: list.into_iter().map(Into::into).collect(),
    }))
}

#[axum::debug_handler]
#[utoipa::path(
    post,
    path = "/installations/{installation_id}/claim",
    tag = "GitHub",
    summary = "installation をテナントに紐付ける",
    description = "対象テナントの admin 以上が必要。既に同じテナントが claim 済みなら冪等に成功する。",
    params(("installation_id" = i64, Path, description = "GitHub の installation ID")),
    request_body = ClaimInstallationRequest,
    responses(
        (status = 200, description = "claim した installation", body = GithubInstallationResponse),
        (status = 409, description = "他のテナントが claim 済みです", body = ServerError),
        CrudErrors,
    )
)]
pub async fn claim_installation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(installation_id): Path<i64>,
    Json(payload): Json<ClaimInstallationRequest>,
) -> Result<Json<GithubInstallationResponse>, AppError> {
    auth.require_session()?;
    tenant_service::require_role(
        &state.db,
        payload.tenant_id,
        auth.user_id,
        TenantRole::Admin,
    )
    .await?;

    let claimed =
        github_service::claim_installation(&state.db, installation_id, payload.tenant_id).await?;
    Ok(Json(claimed.into()))
}

// ── /v1/projects/{project_id}/github ────────────────────────────────────────

#[axum::debug_handler]
#[utoipa::path(
    patch,
    path = "/{project_id}/github",
    tag = "GitHub",
    summary = "プロジェクトの GitHub 連携を設定 / 解除",
    description = "admin 以上が必要。`installation_id` と `github_repo` は常にセットで指定する \
                   （両方 `null` なら解除）。installation はプロジェクトと同じテナントが \
                   claim 済みでなければならない。",
    params(("project_id" = Uuid, Path, description = "プロジェクトID")),
    request_body = UpdateProjectGithubRequest,
    responses(
        (status = 200, description = "更新後のプロジェクト", body = ProjectResponse),
        (status = 400, description = "リポジトリ形式が不正 / installation が存在しません", body = ServerError),
        CrudErrors,
    )
)]
pub async fn update_project_github(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<UpdateProjectGithubRequest>,
) -> Result<Json<ProjectResponse>, AppError> {
    auth.require_session()?;
    let project = crate::handlers::builds::load_project_with_role(
        &state,
        project_id,
        auth.user_id,
        TenantRole::Admin,
    )
    .await?;

    let link = match (payload.installation_id, payload.github_repo) {
        (Some(installation_id), Some(repo)) => Some((installation_id, repo)),
        (None, None) => None,
        _ => {
            return Err(AppError::BadRequestDetail(
                "installation_id and github_repo must be set together".into(),
            ));
        }
    };

    let updated = github_service::set_project_github_link(&state.db, project, link).await?;
    Ok(Json(updated.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "webhook-secret";
    const BODY: &[u8] = br#"{"action":"created"}"#;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn accepts_matching_signature() {
        assert!(verify_webhook_signature(SECRET, &sign(SECRET, BODY), BODY));
    }

    #[test]
    fn rejects_signature_from_other_secret() {
        assert!(!verify_webhook_signature(
            SECRET,
            &sign("other-secret", BODY),
            BODY
        ));
    }

    #[test]
    fn rejects_signature_for_other_body() {
        assert!(!verify_webhook_signature(
            SECRET,
            &sign(SECRET, b"{}"),
            BODY
        ));
    }

    #[test]
    fn rejects_missing_prefix() {
        let signature = sign(SECRET, BODY);
        let bare = signature.strip_prefix("sha256=").unwrap();
        assert!(!verify_webhook_signature(SECRET, bare, BODY));
        assert!(!verify_webhook_signature(
            SECRET,
            &format!("sha1={bare}"),
            BODY
        ));
    }

    #[test]
    fn rejects_non_hex_and_truncated_digests() {
        assert!(!verify_webhook_signature(SECRET, "sha256=zzzz", BODY));
        // 長さ違いでも定数時間比較が false を返す。
        assert!(!verify_webhook_signature(SECRET, "sha256=00ff", BODY));
        assert!(!verify_webhook_signature(SECRET, "sha256=", BODY));
    }
}
