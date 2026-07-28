use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

use entity::{personal_tokens, scopes::Scope, scopes::ScopeList};

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct CreatePersonalTokenRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    /// 付与するスコープ（`read:project` / `write:build` / `read:build`）。
    #[validate(length(min = 1, message = "at least one scope is required"))]
    pub scopes: Vec<Scope>,
    #[schema(value_type = String, format = "date-time", nullable)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// PAT のメタデータ（平文トークン・ハッシュは含まない）
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PersonalTokenResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub name: String,
    pub token_last_four: String,
    pub scopes: ScopeList,
    #[schema(value_type = String, format = "date-time", nullable)]
    pub expires_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = "date-time", nullable)]
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked: bool,
    #[schema(value_type = String, format = "uuid")]
    pub user_id: Uuid,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
}

impl From<personal_tokens::Model> for PersonalTokenResponse {
    fn from(model: personal_tokens::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            token_last_four: model.token_last_four,
            scopes: model.scopes,
            expires_at: model.expires_at.map(|dt| dt.with_timezone(&Utc)),
            last_used_at: model.last_used_at.map(|dt| dt.with_timezone(&Utc)),
            revoked: model.revoked,
            user_id: model.user_id,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}

/// PAT 作成時のレスポンス（平文トークンはこの応答でのみ返却）
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatePersonalTokenResponse {
    /// 平文トークン。以後は取得できないため呼び出し側で保管すること。
    pub token: String,
    #[serde(flatten)]
    pub metadata: PersonalTokenResponse,
}

impl CreatePersonalTokenResponse {
    pub fn new(token: String, model: personal_tokens::Model) -> Self {
        Self {
            token,
            metadata: PersonalTokenResponse::from(model),
        }
    }
}
