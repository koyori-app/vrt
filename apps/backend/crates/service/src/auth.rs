//! 認証まわりのサービス層。
//!
//! - パーソナルアクセストークン (PAT) の生成・検証（task から移植）
//! - OAuth ログイン結果からのユーザー upsert
//!
//! パスワード認証は VRT には存在しない（GitHub / GitLab OAuth のみ）。

use auth_core::provider::ProviderUserInfo;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::Sha256;
use thiserror::Error;
use tracing::error;

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    prelude::Uuid,
};

use crate::error::{ServerError, internal_server_error};
use common::db::{is_postgres_unique_violation, with_transaction};
use entity::personal_tokens::{self, Entity as PersonalTokenEntity};
use entity::{oauth_connections, users};

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("no such user")]
    UserNotFound,
    #[error("bad request")]
    BadRequest,
}

impl From<sea_orm::DbErr> for AuthError {
    fn from(err: sea_orm::DbErr) -> Self {
        AuthError::Internal(err.into())
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        match self {
            AuthError::Internal(e) => {
                error!("auth error: {:#?}", e);
                internal_server_error().into_response()
            }
            AuthError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(ServerError {
                    message: "unauthorized".into(),
                }),
            )
                .into_response(),
            AuthError::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(ServerError {
                    message: "forbidden".into(),
                }),
            )
                .into_response(),
            AuthError::UserNotFound => (
                StatusCode::NOT_FOUND,
                Json(ServerError {
                    message: "not-found".into(),
                }),
            )
                .into_response(),
            AuthError::BadRequest => (
                StatusCode::BAD_REQUEST,
                Json(ServerError {
                    message: "bad-request".into(),
                }),
            )
                .into_response(),
        }
    }
}

// --- Personal token helpers（task の実装をそのまま移植）---

type HmacSha256 = Hmac<Sha256>;

/// PAT の接頭辞。ログ等での識別と、シークレットスキャナ向けの目印を兼ねる。
pub const PERSONAL_TOKEN_PREFIX: &str = "pat_";

/// `pat_<base64url>` 形式のトークンと、DB に保存する HMAC ハッシュを返す。
pub fn generate_personal_token(secret: &str) -> Result<(String, String), AuthError> {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    let token = format!("{PERSONAL_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(buf));
    let token_hash = create_personal_token_hash(&token, secret)?;
    Ok((token, token_hash))
}

/// サーバー側で保持するトークンのハッシュを作る。
/// HMAC-SHA256(secret, token) を Base64URL でエンコードして返す。
pub fn create_personal_token_hash(token: &str, secret: &str) -> Result<String, AuthError> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| AuthError::Internal(anyhow::anyhow!("hmac init: {e}")))?;
    mac.update(token.as_bytes());
    let result = mac.finalize().into_bytes();
    Ok(URL_SAFE_NO_PAD.encode(result.as_slice()))
}

/// 表示用の下 4 桁。
pub fn token_last_four(token: &str) -> String {
    token[token.len().saturating_sub(4)..].to_string()
}

/// DB から取得した PAT レコード（認証成功時）。
pub type PersonalTokenRecord = personal_tokens::Model;

/// Bearer トークンを検証し、有効な PAT レコードを返す。
pub async fn authenticate_personal_token(
    db: &DatabaseConnection,
    secret: &str,
    token_plaintext: &str,
) -> Result<PersonalTokenRecord, AuthError> {
    let token_hash = create_personal_token_hash(token_plaintext, secret)?;

    let token = PersonalTokenEntity::find()
        .filter(personal_tokens::Column::TokenHash.eq(token_hash))
        .one(db)
        .await?
        .ok_or(AuthError::Unauthorized)?;

    if token.revoked {
        return Err(AuthError::Unauthorized);
    }

    if let Some(expires) = &token.expires_at
        && expires < &Utc::now().fixed_offset()
    {
        return Err(AuthError::Unauthorized);
    }

    Ok(token)
}

/// PAT の最終利用時刻を更新する。認証経路で毎リクエスト呼ばれる。
pub async fn touch_personal_token_last_used(
    db: &DatabaseConnection,
    token_id: Uuid,
) -> Result<(), AuthError> {
    personal_tokens::Entity::update_many()
        .col_expr(
            personal_tokens::Column::LastUsedAt,
            sea_orm::sea_query::Expr::value(Utc::now().fixed_offset()),
        )
        .filter(personal_tokens::Column::Id.eq(token_id))
        .exec(db)
        .await?;
    Ok(())
}

// --- OAuth ユーザー ---

/// OAuth のアクセストークンを暗号化する（AES-256-GCM / auth-core crypto）。
pub fn encrypt_oauth_token(key_material: &str, token: &str) -> Result<String, AuthError> {
    auth_core::crypto::encrypt_token(key_material, token).map_err(AuthError::Internal)
}

/// OAuth のアクセストークンを復号する。
pub fn decrypt_oauth_token(key_material: &str, encoded: &str) -> Result<String, AuthError> {
    auth_core::crypto::decrypt_token(key_material, encoded).map_err(AuthError::Internal)
}

/// ユーザー名候補の正規化。プロバイダーの username をそのまま使えないケースに備える。
fn normalize_username(raw: &str) -> String {
    let normalized: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();

    if normalized.is_empty() {
        "user".to_string()
    } else {
        normalized
    }
}

/// 衝突時のサフィックス付き候補（1 回目は素の base）。
fn username_candidate(base: &str, attempt: usize) -> String {
    if attempt == 0 {
        return base.to_string();
    }
    let suffix = format!("-{attempt}");
    let head_len = base.len().min(32 - suffix.len());
    format!("{}{suffix}", &base[..head_len])
}

const USERNAME_ATTEMPTS: usize = 8;

/// OAuth ログイン結果からユーザーを解決する。
///
/// 1. `(provider, provider_user_id)` で既存の連携を探し、あればそのユーザーを返す
/// 2. 無ければユーザーと連携をトランザクションで新規作成する
///
/// `access_token_enc` は暗号化済みのアクセストークン（[`encrypt_oauth_token`]）。
pub async fn upsert_oauth_user(
    db: &DatabaseConnection,
    provider_slug: &str,
    info: &ProviderUserInfo,
    access_token_enc: Option<String>,
) -> Result<users::Model, AuthError> {
    if let Some(connection) = find_connection(db, provider_slug, &info.provider_user_id).await? {
        if let Some(enc) = access_token_enc.clone() {
            let mut active: oauth_connections::ActiveModel = connection.clone().into();
            active.access_token_enc = Set(Some(enc));
            active.update(db).await?;
        }
        return users::Entity::find_by_id(connection.user_id)
            .one(db)
            .await?
            .ok_or(AuthError::UserNotFound);
    }

    let base = normalize_username(&info.username);

    for attempt in 0..USERNAME_ATTEMPTS {
        let username = username_candidate(&base, attempt);

        // 事前チェックで無駄なトランザクションのロールバックを減らす
        // （最終的な一意性は DB のユニーク制約が保証する）。
        if username_taken(db, &username).await? {
            continue;
        }

        let result = create_user_with_connection(
            db,
            provider_slug,
            info,
            &username,
            access_token_enc.clone(),
        )
        .await;

        match result {
            Ok(user) => return Ok(user),
            Err(AuthError::Internal(e)) => {
                // 連携側の一意制約違反 = 並行リクエストが同じ OAuth アカウントを登録した。
                // その場合は既存の連携を読み直して返す。
                if let Some(db_err) = e.downcast_ref::<sea_orm::DbErr>()
                    && is_postgres_unique_violation(db_err)
                {
                    if let Some(connection) =
                        find_connection(db, provider_slug, &info.provider_user_id).await?
                    {
                        return users::Entity::find_by_id(connection.user_id)
                            .one(db)
                            .await?
                            .ok_or(AuthError::UserNotFound);
                    }
                    // username の衝突なら次の候補で再試行。
                    continue;
                }
                return Err(AuthError::Internal(e));
            }
            Err(other) => return Err(other),
        }
    }

    Err(AuthError::Internal(anyhow::anyhow!(
        "could not allocate a unique username for oauth user"
    )))
}

/// **テスト専用。** ユーザー名だけでユーザーを取得または作成する。
///
/// OAuth 連携を持たないユーザーを作るため、e2e のセッション種付け以外では使わない。
/// 呼び出し側（`handlers::auth::test_login`）が `TEST_LOGIN_ENABLED` を検査してから使う。
pub async fn upsert_test_user(
    db: &DatabaseConnection,
    username: &str,
) -> Result<users::Model, AuthError> {
    let username = normalize_username(username);

    if let Some(existing) = users::Entity::find()
        .filter(users::Column::Username.eq(username.clone()))
        .one(db)
        .await?
    {
        return Ok(existing);
    }

    let now = Utc::now().fixed_offset();
    match (users::ActiveModel {
        id: Set(Uuid::new_v4()),
        username: Set(username.clone()),
        display_name: Set(username.clone()),
        avatar_url: Set(None),
        email: Set(None),
        sessions_revoked_at: Set(None),
        // 表示言語は未設定で作る——画面はブラウザの言語に従い、
        // ユーザーが明示的に選んだときだけ固定される。
        language: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await)
    {
        Ok(user) => Ok(user),
        // 並行実行で同じユーザー名を作った場合は既存行を読み直す。
        Err(e) if is_postgres_unique_violation(&e) => users::Entity::find()
            .filter(users::Column::Username.eq(username))
            .one(db)
            .await?
            .ok_or(AuthError::UserNotFound),
        Err(e) => Err(AuthError::Internal(e.into())),
    }
}

async fn find_connection(
    db: &DatabaseConnection,
    provider_slug: &str,
    provider_user_id: &str,
) -> Result<Option<oauth_connections::Model>, AuthError> {
    Ok(oauth_connections::Entity::find()
        .filter(oauth_connections::Column::Provider.eq(provider_slug))
        .filter(oauth_connections::Column::ProviderUserId.eq(provider_user_id))
        .one(db)
        .await?)
}

async fn username_taken(db: &DatabaseConnection, username: &str) -> Result<bool, AuthError> {
    Ok(users::Entity::find()
        .filter(users::Column::Username.eq(username))
        .one(db)
        .await?
        .is_some())
}

async fn create_user_with_connection(
    db: &DatabaseConnection,
    provider_slug: &str,
    info: &ProviderUserInfo,
    username: &str,
    access_token_enc: Option<String>,
) -> Result<users::Model, AuthError> {
    let provider_slug = provider_slug.to_string();
    let provider_user_id = info.provider_user_id.clone();
    let username = username.to_string();
    let display_name = if info.username.trim().is_empty() {
        username.clone()
    } else {
        info.username.trim().to_string()
    };
    // 未検証メールは信頼しない（他人のメールを詐称したアカウントとの突合を防ぐ）。
    let email = match info.email_verified {
        Some(true) => info.email.clone(),
        _ => None,
    };
    let avatar_url = info.avatar_url.clone();

    with_transaction(db, move |txn| {
        Box::pin(async move {
            let now = Utc::now().fixed_offset();
            let user = users::ActiveModel {
                id: Set(Uuid::new_v4()),
                username: Set(username),
                display_name: Set(display_name),
                avatar_url: Set(avatar_url),
                email: Set(email),
                sessions_revoked_at: Set(None),
                language: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(txn)
            .await?;

            oauth_connections::ActiveModel {
                id: Set(Uuid::new_v4()),
                user_id: Set(user.id),
                provider: Set(provider_slug),
                provider_user_id: Set(provider_user_id),
                access_token_enc: Set(access_token_enc),
                created_at: Set(now),
            }
            .insert(txn)
            .await?;

            Ok(user)
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "00000000000000000000000000000000";

    #[test]
    fn generated_token_uses_prefix_and_is_stable_under_hashing() {
        let (token, hash) = generate_personal_token(SECRET).unwrap();
        assert!(token.starts_with(PERSONAL_TOKEN_PREFIX));
        assert_eq!(create_personal_token_hash(&token, SECRET).unwrap(), hash);
    }

    #[test]
    fn hash_differs_with_a_different_secret() {
        let (token, hash) = generate_personal_token(SECRET).unwrap();
        let other = create_personal_token_hash(&token, "11111111111111111111111111111111").unwrap();
        assert_ne!(hash, other);
    }

    #[test]
    fn last_four_is_the_tail_of_the_token() {
        assert_eq!(token_last_four("pat_abcdef"), "cdef");
        assert_eq!(token_last_four("ab"), "ab");
    }

    #[test]
    fn username_is_normalized_to_a_safe_slug() {
        assert_eq!(normalize_username("Yupi X"), "yupi_x");
        assert_eq!(normalize_username("  "), "user");
        assert_eq!(normalize_username("ok-name_1"), "ok-name_1");
        assert!(normalize_username(&"a".repeat(100)).len() <= 32);
    }

    #[test]
    fn username_candidates_stay_within_the_column_limit() {
        let base = "a".repeat(32);
        let candidate = username_candidate(&base, 3);
        assert!(candidate.len() <= 32);
        assert!(candidate.ends_with("-3"));
        assert_eq!(username_candidate("bob", 0), "bob");
    }
}
