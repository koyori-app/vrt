//! ビジネスロジック/横断サービス層。

// 旧 crate::error / crate::settings パス互換のための再公開。
pub use common::{error, settings};

pub mod auth;
pub mod baselines;
pub mod builds;
pub mod comparisons;
pub mod diff;
pub mod github;
pub mod http;
pub mod oauth;
pub mod oauth_state;
pub mod projects;
pub mod render;
pub mod screenshots;
pub mod storage;
pub mod tenants;

pub use common::db;
pub use common::validation;
