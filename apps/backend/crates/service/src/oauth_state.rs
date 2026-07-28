//! OAuth state の保存先（Redis 実装）。
//!
//! [`auth_core::StateStore`] の契約どおり `consume` は取得と削除を原子的に行う必要がある。
//! Redis の `GETDEL`（6.2+ / Valkey 8）を使うことで state のリプレイを防ぐ。

use async_trait::async_trait;
use auth_core::StateStore;
use common::cache::redis::RedisConnection;

#[derive(Clone)]
pub struct RedisStateStore {
    redis: RedisConnection,
}

impl RedisStateStore {
    pub fn new(redis: RedisConnection) -> Self {
        Self { redis }
    }
}

#[async_trait]
impl StateStore for RedisStateStore {
    async fn store(&self, key: &str, value: &str, ttl_secs: u64) -> Result<(), anyhow::Error> {
        let mut conn = self
            .redis
            .conn
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("redis acquire failed: {e}"))?;

        redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("EX")
            .arg(ttl_secs)
            .exec_async(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("redis SET failed: {e}"))?;

        Ok(())
    }

    async fn consume(&self, key: &str) -> Result<Option<String>, anyhow::Error> {
        let mut conn = self
            .redis
            .conn
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("redis acquire failed: {e}"))?;

        // GETDEL は取得と削除を 1 コマンドで行うため、並行リクエストで
        // 同じ state を 2 回消費できない（リプレイ防止）。
        let value: Option<String> = redis::cmd("GETDEL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("redis GETDEL failed: {e}"))?;

        Ok(value)
    }
}
