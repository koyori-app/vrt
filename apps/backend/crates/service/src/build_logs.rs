//! ビルド進捗ログの追記・取得。
//!
//! render / compare のジョブが要所で行を追記し、UI と CI が `?after=<id>` で
//! 増分取得する。`build_logs.id` は BIGSERIAL のグローバル連番で、これが
//! そのままカーソルになる（クライアントは「前回見た最後の id」を送り返すだけ）。
//!
//! ## エラー方針
//!
//! [`append`] は通常の `Result` を返し、呼び出し側のジョブはこれを `?` で
//! 伝播させる（`let _ =` で握り潰さない）。ログが書けない状態は DB 障害であり、
//! そのままジョブを継続しても比較・レンダリング結果を書き戻せないため、
//! ログの書き込み失敗はジョブの失敗として扱うのが正しい。

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ActiveValue::Set, ColumnTrait, ConnectionTrait,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, prelude::Uuid,
};

use common::error::AppError;
use entity::build_logs;

/// 1 回の取得で返す最大行数。UI/CI のポーリングが 1 リクエストで
/// 過大な行を引かないための上限。
pub const MAX_LIST_LIMIT: usize = 1000;

/// ログの重大度。DB には小文字の文字列で入る（`level TEXT`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// DB に保存する文字列表現。
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// 1 行のログを追記する。
///
/// `id` は DB 側の BIGSERIAL に任せる（`NotSet`）。挿入された行をそのまま返すので、
/// 呼び出し側が採番された id を知りたければ利用できる。
pub async fn append<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
    level: LogLevel,
    message: impl Into<String>,
) -> Result<build_logs::Model, AppError> {
    Ok(build_logs::ActiveModel {
        id: NotSet,
        build_id: Set(build_id),
        level: Set(level.as_str().to_string()),
        message: Set(message.into()),
        created_at: Set(Utc::now().fixed_offset()),
    }
    .insert(db)
    .await?)
}

/// `after_id` より大きい id のログを id 昇順で取得する。
///
/// `limit` は [`MAX_LIST_LIMIT`] に丸める。`after_id = 0`（省略時の既定）は
/// 先頭からの取得を意味する（id は 1 始まり）。
pub async fn list_after<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
    after_id: i64,
    limit: usize,
) -> Result<Vec<build_logs::Model>, AppError> {
    Ok(build_logs::Entity::find()
        .filter(build_logs::Column::BuildId.eq(build_id))
        .filter(build_logs::Column::Id.gt(after_id))
        .order_by_asc(build_logs::Column::Id)
        .limit(clamp_limit(limit) as u64)
        .all(db)
        .await?)
}

/// `limit` を 1..=[`MAX_LIST_LIMIT`] に収める。
fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_LIST_LIMIT)
}

/// 取得結果からクライアントに返す次のカーソル（`last_id`）を決める。
///
/// 行が無ければリクエストされた `after_id` を据え置く（クライアントは同じ
/// カーソルで次を待てばよい）。あれば末尾行の id。id 昇順が保証されているので
/// 末尾が最大。
pub fn resolve_last_id(after_id: i64, entries: &[build_logs::Model]) -> i64 {
    entries.last().map(|e| e.id).unwrap_or(after_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_serialize_to_lowercase() {
        assert_eq!(LogLevel::Info.as_str(), "info");
        assert_eq!(LogLevel::Warn.as_str(), "warn");
        assert_eq!(LogLevel::Error.as_str(), "error");
    }

    #[test]
    fn limit_is_clamped_to_bounds() {
        assert_eq!(clamp_limit(0), 1);
        assert_eq!(clamp_limit(50), 50);
        assert_eq!(clamp_limit(MAX_LIST_LIMIT), MAX_LIST_LIMIT);
        assert_eq!(clamp_limit(MAX_LIST_LIMIT + 1), MAX_LIST_LIMIT);
        assert_eq!(clamp_limit(usize::MAX), MAX_LIST_LIMIT);
    }

    fn row(id: i64) -> build_logs::Model {
        build_logs::Model {
            id,
            build_id: Uuid::new_v4(),
            level: "info".into(),
            message: "m".into(),
            created_at: Utc::now().fixed_offset(),
        }
    }

    #[test]
    fn last_id_advances_with_rows_and_holds_when_empty() {
        // 行が無ければカーソルは据え置き。
        assert_eq!(resolve_last_id(42, &[]), 42);
        // 行があれば末尾（= 最大 id）に進む。
        assert_eq!(resolve_last_id(0, &[row(1), row(2), row(3)]), 3);
        assert_eq!(resolve_last_id(5, &[row(6)]), 6);
    }
}
