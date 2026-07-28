//! `github_installations` entity — schema-first generated output re-exported for stable module path.
//!
//! GitHub App の installation を 1 行 1 件で保持する。行を作るのは webhook
//! (`installation.created`) だけで、テナントへの紐付け（claim）は後から
//! `POST /v1/github/installations/{installation_id}/claim` が行う。
pub use super::_generated::github_installations::*;

/// アカウント種別の既定値。webhook のペイロードに `account.type` が無いときに使う。
pub const DEFAULT_ACCOUNT_TYPE: &str = "Organization";
