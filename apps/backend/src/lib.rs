//! バイナリ用の薄い glue クレート。実装は各ワークスペースクレートに分割:
//! entity → common → payload → service → job → handler → backend(bin)
//!
//! export_openapi 等からのパス互換のため再エクスポートを維持する。

pub mod server;

pub use common::{error, settings};
pub use handler::{AppState, handlers, middlewares, openapi, routes};
pub use job as jobs;
pub use service as utils;
