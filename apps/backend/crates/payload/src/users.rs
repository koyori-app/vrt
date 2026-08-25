use crate::serde_ext::double_option;
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 画面表示に使う言語。
///
/// 未設定（`None`）は「クライアントに任せる」——`Accept-Language` や
/// ブラウザ設定から決める。ここに無い言語タグが DB に入っていた場合も
/// 未設定として扱う（不正な行でプロフィール取得ごと落とさない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// English
    En,
    /// 日本語
    Ja,
}

impl Language {
    /// DB へ入れる言語タグ。
    pub fn as_str(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Ja => "ja",
        }
    }

    /// DB の値を読む。対応していないタグは未設定として扱う。
    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "en" => Some(Language::En),
            "ja" => Some(Language::Ja),
            _ => None,
        }
    }
}

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
    /// 表示言語。`null` は未設定（クライアント側の判定に任せる）。
    #[schema(nullable)]
    pub language: Option<Language>,
}

/// プロフィールの更新。
///
/// `language` に `null` を送ると未設定へ戻る（＝ブラウザの言語に従う）。
/// フィールド自体を省略したときは現在値を据え置く——他プロジェクト設定の
/// PATCH と同じ約束にしてある。対応していない言語タグは enum の
/// deserialize が弾く。
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateMeRequest {
    /// 表示言語。`null` で未設定へ戻す。省略すると据え置き。
    #[schema(nullable, value_type = Option<Language>)]
    #[serde(default, deserialize_with = "double_option")]
    pub language: Option<Option<Language>>,
}

impl From<entity::users::Model> for MeResponse {
    fn from(model: entity::users::Model) -> Self {
        Self {
            id: model.id,
            username: model.username,
            display_name: model.display_name,
            avatar_url: model.avatar_url,
            email: model.email,
            language: model.language.as_deref().and_then(Language::from_stored),
        }
    }
}
