//! VRT CLI のコアロジック。
//!
//! バイナリ（`vrt`）から使うモジュールを公開する。TurboSnap 相当の
//! 影響ストーリー算出（`turbosnap`）は純関数群で、tests/ から直接叩ける。

pub mod api;
pub mod bundle;
pub mod git;
pub mod plan;
pub mod provenance;
pub mod turbosnap;

// テスト専用（一時 git リポジトリ初期化）。lib の unit test には `test`、
// bin の unit test と統合テストには self dev-dependency の `test-support` で見せる。
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
