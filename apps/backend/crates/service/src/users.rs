//! ログイン中ユーザー自身のプロフィール操作。

use chrono::Utc;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, EntityTrait, prelude::Uuid};

use common::error::AppError;
use entity::users;

/// ユーザーを引く。存在しなければ [`AppError::NotFound`]。
pub async fn get_user<C: ConnectionTrait>(db: &C, user_id: Uuid) -> Result<users::Model, AppError> {
    users::Entity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// 表示言語を差し替える。
///
/// `language` に `None` を渡すと未設定へ戻る——画面はブラウザの言語に従う。
/// 呼び出し側で対応言語かどうかは検証済み（payload の enum が担う）。
pub async fn set_language<C: ConnectionTrait>(
    db: &C,
    user: users::Model,
    language: Option<String>,
) -> Result<users::Model, AppError> {
    let mut active: users::ActiveModel = user.into();
    active.language = Set(language);
    active.updated_at = Set(Utc::now().into());
    Ok(active.update(db).await?)
}
