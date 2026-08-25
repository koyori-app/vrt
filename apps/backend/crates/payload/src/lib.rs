//! リクエスト/レスポンス DTO（payload クレート）。依存は entity / common のみに閉じる。

pub(crate) mod serde_ext;

pub mod builds;
pub mod comparisons;
pub mod github;
pub mod personal_tokens;
pub mod projects;
pub mod tenants;
pub mod users;
