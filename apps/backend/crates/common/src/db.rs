//! データベースまわりの共通ヘルパ。
//!
//! トランザクションは SeaORM の [`TransactionTrait::transaction`] を使う。
//! 公式: <https://www.sea-ql.org/SeaORM/docs/advanced-query/transaction/>

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, ExecResult,
    Statement, TransactionError, TransactionTrait, Value,
};

/// アプリ用 SeaORM プールの既定上限。
///
/// SeaORM は `max_connections` 未指定だと sqlx の既定（10）に委ねる。
/// 明示しておかないと「1 プロセスあたり何本まで掴むか」が暗黙になり、
/// 1 プロセスで複数のアプリインスタンスを立てる統合テストで
/// Postgres の `max_connections` を食い潰して `PoolTimedOut` になる。
pub const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;

/// 環境変数 `DATABASE_MAX_CONNECTIONS` から上限を読む（未設定/不正なら既定値）。
pub fn db_max_connections() -> u32 {
    std::env::var("DATABASE_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_DB_MAX_CONNECTIONS)
}

/// 上限を明示した SeaORM 接続を張る。
///
/// `Database::connect(url)` を直接呼ぶとプール上限が暗黙(10)になるため、
/// アプリ・テストとも必ずこちらを通す。
pub async fn connect_database(url: &str) -> Result<DatabaseConnection, DbErr> {
    let mut options = ConnectOptions::new(url.to_owned());
    options
        .max_connections(db_max_connections())
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(30))
        .sqlx_logging(false);
    Database::connect(options).await
}

/// 一意制約違反（重複キーなど）として扱ってよさそうなエラーか。
pub fn is_postgres_unique_violation(err: &DbErr) -> bool {
    matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    )
}

/// [`DatabaseConnection::transaction`] の薄いラッパ。
///
/// クロージャはドキュメントどおり `Box::pin(async move { ... })` を返す。
/// `Ok` で commit、`Err` で rollback する。
pub async fn with_transaction<T, E, F>(db: &DatabaseConnection, f: F) -> Result<T, E>
where
    F: for<'c> FnOnce(
            &'c sea_orm::DatabaseTransaction,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>
        + Send,
    T: Send,
    E: std::fmt::Display + std::fmt::Debug + Send + From<DbErr>,
{
    db.transaction(f).await.map_err(|e| match e {
        TransactionError::Connection(err) => E::from(err),
        TransactionError::Transaction(err) => err,
    })
}

/// `?` プレースホルダをバックエンド固有の記法に変換する（Postgres は `$1, $2, ...`）。
///
/// SeaORM の [`Statement::from_sql_and_values`] は SQL を無変換で sqlx に渡すため、
/// Postgres では `?` のままだと実行時に構文エラーになる。
///
/// # 制約
///
/// char 単位の無条件置換のため、テンプレート中の `?` は**バインドプレースホルダ専用**。
/// Postgres の jsonb 演算子（`?` / `?|` / `?&`）や文字列リテラル中の `?` を含む SQL を
/// 渡してはならない（それらも `$N` に化けて壊れる）。jsonb のキー存在判定が必要な場合は
/// この関数を通さず `$N` を直接書くか、`jsonb_exists()` 関数形式を使うこと。
pub fn bind_sql(backend: DbBackend, template: &str) -> String {
    if backend != DbBackend::Postgres {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut index = 0;
    for ch in template.chars() {
        if ch == '?' {
            index += 1;
            out.push('$');
            out.push_str(&index.to_string());
        } else {
            out.push(ch);
        }
    }
    out
}

/// `?` プレースホルダの SQL を値バインド付きで実行する。
pub async fn execute_bound<C: ConnectionTrait>(
    conn: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<ExecResult, DbErr> {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        bind_sql(backend, sql),
        values,
    ))
    .await
}

/// `?` プレースホルダの SQL を実行し、先頭カラムを bool として返す
/// （行なし・型不一致は `false`）。
pub async fn query_one_bool<C: ConnectionTrait>(
    conn: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<bool, DbErr> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            bind_sql(backend, sql),
            values,
        ))
        .await?;
    Ok(row
        .and_then(|r| r.try_get_by_index::<bool>(0).ok())
        .unwrap_or(false))
}

/// public スキーマにテーブルが存在するか。
pub async fn table_exists<C: ConnectionTrait>(conn: &C, table: &str) -> Result<bool, DbErr> {
    query_one_bool(
        conn,
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_schema = 'public' AND table_name = ?
        )",
        vec![table.into()],
    )
    .await
}

/// public スキーマのテーブルにカラムが存在するか。
pub async fn column_exists<C: ConnectionTrait>(
    conn: &C,
    table: &str,
    column: &str,
) -> Result<bool, DbErr> {
    query_one_bool(
        conn,
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.columns
            WHERE table_schema = 'public' AND table_name = ? AND column_name = ?
        )",
        vec![table.into(), column.into()],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_sql_converts_placeholders_for_postgres() {
        assert_eq!(
            bind_sql(DbBackend::Postgres, "SELECT a FROM t WHERE b = ? AND c = ?"),
            "SELECT a FROM t WHERE b = $1 AND c = $2"
        );
    }

    #[test]
    fn bind_sql_keeps_placeholders_for_other_backends() {
        assert_eq!(
            bind_sql(DbBackend::Sqlite, "SELECT a FROM t WHERE b = ?"),
            "SELECT a FROM t WHERE b = ?"
        );
    }
}
