use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

use entity::{tenant_members, tenant_members::TenantRole, tenants, users};

// ── tenants ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TenantResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub name: String,
    /// URL 断片。小文字英数とハイフンのみ。
    pub slug: String,
    #[schema(nullable)]
    pub avatar_url: Option<String>,
    /// 呼び出し元自身のこのテナントにおけるロール。
    ///
    /// メンバーシップを解決したうえで返すエンドポイント（一覧・取得・作成・更新）では
    /// 必ず入る。メンバーシップが分からない文脈でのみ `null`。
    #[schema(nullable)]
    pub my_role: Option<TenantRole>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: DateTime<Utc>,
}

impl TenantResponse {
    /// 呼び出し元のロールを添えて組み立てる。
    pub fn with_role(model: tenants::Model, my_role: TenantRole) -> Self {
        Self {
            my_role: Some(my_role),
            ..Self::from(model)
        }
    }
}

impl From<tenants::Model> for TenantResponse {
    fn from(model: tenants::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            slug: model.slug,
            avatar_url: model.avatar_url,
            my_role: None,
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct CreateTenantRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    /// 小文字英数とハイフンのみ。予約語は使用できない。
    #[validate(length(min = 2, max = 63))]
    pub slug: String,
    #[validate(url)]
    pub avatar_url: Option<String>,
}

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct UpdateTenantRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    #[validate(url)]
    pub avatar_url: Option<String>,
    /// `true` なら `avatar_url` を明示的に削除する。
    #[serde(default)]
    pub clear_avatar_url: bool,
}

// ── members ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TenantMemberResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub tenant_id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub user_id: Uuid,
    pub role: TenantRole,
    /// 表示用のユーザー名（`users` を join して埋める）。join できなかった場合のみ `null`。
    #[schema(nullable)]
    pub username: Option<String>,
    #[schema(nullable)]
    pub display_name: Option<String>,
    #[schema(nullable)]
    pub avatar_url: Option<String>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
}

impl From<tenant_members::Model> for TenantMemberResponse {
    fn from(model: tenant_members::Model) -> Self {
        Self {
            id: model.id,
            tenant_id: model.tenant_id,
            user_id: model.user_id,
            role: model.role,
            username: None,
            display_name: None,
            avatar_url: None,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}

/// `list_members` が返す join 済みの行。
impl From<(tenant_members::Model, Option<users::Model>)> for TenantMemberResponse {
    fn from((member, user): (tenant_members::Model, Option<users::Model>)) -> Self {
        let (username, display_name, avatar_url) = match user {
            Some(u) => (Some(u.username), Some(u.display_name), u.avatar_url),
            None => (None, None, None),
        };
        Self {
            username,
            display_name,
            avatar_url,
            ..Self::from(member)
        }
    }
}

/// メンバー追加。`user_id` と `username` はどちらか一方を指定する。
#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct AddMemberRequest {
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub user_id: Option<Uuid>,
    #[validate(length(min = 1))]
    pub username: Option<String>,
    pub role: TenantRole,
}

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct UpdateMemberRequest {
    pub role: TenantRole,
}
