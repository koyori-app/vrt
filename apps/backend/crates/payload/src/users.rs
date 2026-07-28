use sea_orm::prelude::Uuid;
use serde::Serialize;
use utoipa::ToSchema;

/// ログイン中ユーザー自身のプロフィール。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MeResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    #[schema(nullable)]
    pub avatar_url: Option<String>,
    #[schema(nullable, value_type = Option<String>, format = "email")]
    pub email: Option<String>,
}

impl From<entity::users::Model> for MeResponse {
    fn from(model: entity::users::Model) -> Self {
        Self {
            id: model.id,
            username: model.username,
            display_name: model.display_name,
            avatar_url: model.avatar_url,
            email: model.email,
        }
    }
}
