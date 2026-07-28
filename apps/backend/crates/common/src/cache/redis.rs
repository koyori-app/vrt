use redis_pool::SingleRedisPool;

#[derive(Clone)]
pub struct RedisConnection {
    pub conn: SingleRedisPool,
}

impl RedisConnection {
    pub fn new(url: &str) -> Self {
        let client = redis::Client::open(url).unwrap();
        let pool = SingleRedisPool::from(client);
        Self { conn: pool }
    }

    pub async fn ping(&self) -> Result<(), anyhow::Error> {
        let mut connection = self
            .conn
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("redis acquire failed: {e}"))?;

        let _: String = redis::cmd("PING")
            .query_async(&mut connection)
            .await
            .map_err(|e| anyhow::anyhow!("redis ping failed: {e}"))?;

        Ok(())
    }
}
