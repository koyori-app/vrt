use crate::{
    sec::encrypt, DatabasePool, Session, SessionConfig, SessionData, SessionError, SessionTimers,
};
use axum::extract::FromRequestParts;
use chrono::{Duration, Utc};
use dashmap::DashMap;
#[cfg(feature = "key-store")]
use fastbloom_rs::Deletable;
#[cfg(feature = "key-store")]
use fastbloom_rs::{CountingBloomFilter, FilterBuilder, Membership};
use http::{request::Parts, StatusCode};
use serde::Serialize;
use std::{
    fmt::Debug,
    hash::{Hash, Hasher},
    sync::Arc,
};
use tokio::sync::{Mutex, RwLock};

/// LOCAL PATCH: セッション ID 単位で DB 操作を直列化するためのロック本数。
///
/// ID ごとに Mutex を作ると、いつ捨ててよいかの判断が要るうえに使い捨ての ID
/// ぶんだけ増え続ける。本数を固定してハッシュで割り当てれば増えないし、衝突しても
/// 別セッションが DB 往復 1 回ぶん待つだけで済む。待つのは tokio の Mutex なので、
/// OS スレッドは解放されたままになる。
const SESSION_LOCK_SHARDS: usize = 64;

/// Contains the main Services storage for all session's and database access for persistent Sessions.
///
/// # Examples
/// ```rust ignore
/// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
///
/// let config = SessionConfig::default();
/// let session_store = SessionStore::<SessionNullPool>::new(None, config).await.unwrap();
/// ```
///
#[derive(Clone, Debug)]
pub struct SessionStore<T>
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    /// Client for the database.
    pub client: Option<T>,
    /// locked Hashmap containing UserID and their session data.
    pub(crate) inner: Arc<DashMap<String, SessionData>>,
    /// Session Configuration.
    pub config: SessionConfig,
    /// Session Timers used for Clearing Memory and Database.
    pub(crate) timers: Arc<RwLock<SessionTimers>>,
    #[cfg(feature = "key-store")]
    /// Filter used to keep track of what session IDs exist.
    pub(crate) filter: Arc<RwLock<CountingBloomFilter>>,
    /// LOCAL PATCH: セッション ID ごとに DB の保存と削除を直列化するロック。
    ///
    /// 保存はメモリ上の値を clone してから await するため、ロックが無いと
    /// 掃除タスクの保存が logout の削除を追い越し、消したはずのセッションを
    /// 書き戻せてしまう（[`Self::store_expired_session`] を参照）。
    pub(crate) locks: Arc<Vec<Mutex<()>>>,
}

impl<T, S> FromRequestParts<S> for SessionStore<T>
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
    S: Send + Sync,
{
    type Rejection = (http::StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let session = parts.extensions.get::<Session<T>>().ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Can't extract Axum `Session`. Is `SessionLayer` enabled?",
        ))?;

        Ok(session.store.clone())
    }
}

impl<T> SessionStore<T>
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    /// Constructs a New `SessionStore` and Creates the Database Table
    /// needed for the Session if it does not exist if client is not `None`.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config).await.unwrap();
    /// ```
    ///
    #[inline]
    pub async fn new(client: Option<T>, config: SessionConfig) -> Result<Self, SessionError> {
        if let Some(client) = &client {
            client.initiate(&config.database.table_name).await?
        }

        // If we have a database client then lets also get any SessionId's that Exist within the database
        // that are not yet expired.
        #[cfg(feature = "key-store")]
        let filter = Self::create_filter(&client, &config).await?;

        Ok(Self {
            client,
            inner: Default::default(),
            config,
            timers: Arc::new(RwLock::new(SessionTimers {
                // the first expiry sweep is scheduled one lifetime from start-up
                last_expiry_sweep: Utc::now() + Duration::try_hours(1).unwrap_or_default(),
                // the first expiry sweep is scheduled one lifetime from start-up
                last_database_expiry_sweep: Utc::now() + Duration::try_hours(6).unwrap_or_default(),
            })),
            #[cfg(feature = "key-store")]
            filter: Arc::new(RwLock::new(filter)),
            locks: Arc::new((0..SESSION_LOCK_SHARDS).map(|_| Mutex::new(())).collect()),
        })
    }

    /// Used to create and Fill the Filter.
    #[cfg(feature = "key-store")]
    pub(crate) async fn create_filter(
        client: &Option<T>,
        config: &SessionConfig,
    ) -> Result<CountingBloomFilter, SessionError> {
        let mut filter = FilterBuilder::new(
            config.memory.filter_expected_elements,
            config.memory.filter_false_positive_probability,
        )
        .build_counting_bloom_filter();

        if config.memory.use_bloom_filters {
            // If client exist then lets preload the id's within the database so the filter is accurate.
            if let Some(client) = &client {
                let ids = client.get_ids(&config.database.table_name).await?;

                ids.iter().for_each(|id| filter.add(id.as_bytes()));
            }
        }

        Ok(filter)
    }

    /// Checks if the database is in persistent mode.
    ///
    /// Returns true if client is Some().
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config).await.unwrap();
    /// let is_persistent = session_store.is_persistent();
    /// ```
    ///
    #[inline]
    pub fn is_persistent(&self) -> bool {
        self.client.is_some()
    }

    /// Cleans Expired sessions from the Database based on Utc::now().
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config).await.unwrap();
    /// async {
    ///     let _ = session_store.cleanup().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn cleanup(&self) -> Result<Vec<String>, SessionError> {
        if let Some(client) = &self.client {
            Ok(client
                .delete_by_expiry(&self.config.database.table_name)
                .await?)
        } else {
            Ok(Vec::new())
        }
    }

    /// Returns count of existing sessions within database.
    ///
    /// If client is None it will return Ok(0).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config).await.unwrap();
    /// async {
    ///     let count = session_store.count().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn count(&self) -> Result<i64, SessionError> {
        if let Some(client) = &self.client {
            let count = client.count(&self.config.database.table_name).await?;
            return Ok(count);
        }

        Ok(0)
    }

    /// private internal function that loads a session's data from the database using an ID string.
    ///
    /// If client is None it will return Ok(None).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    /// - ['SessionError::SerdeJson'] is returned if it failed to deserialize the sessions data.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    /// use uuid::Uuid;
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config).await.unwrap();
    /// let token = Uuid::new_v4();
    /// async {
    ///     let session_data = session_store.load_session(token.to_string()).await.unwrap();
    /// };
    /// ```
    ///
    pub(crate) async fn load_session(
        &self,
        cookie_value: String,
    ) -> Result<Option<SessionData>, SessionError> {
        if let Some(client) = &self.client {
            let result: Option<String> = client
                .load(&cookie_value, &self.config.database.table_name)
                .await?;

            if let Some(mut session) = result
                .map(|session| {
                    if let Some(key) = self.config.database.database_key.as_ref() {
                        serde_json::from_str::<SessionData>(
                            &match encrypt::decrypt(&cookie_value, &session, key) {
                                Ok(v) => v,
                                Err(err) => {
                                    tracing::error!(err = %err, "Failed to decrypt Session data from database.");
                                    String::new()
                                }
                            }
                        )
                    } else {
                        serde_json::from_str::<SessionData>(&session)
                    }
                })
                .transpose()?
            {
                session.id = cookie_value;
                return Ok(Some(session));
            }
        }

        Ok(None)
    }

    /// private internal function that stores a session's data to the database.
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    /// - ['SessionError::SerdeJson'] is returned if it failed to serialize the sessions data.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore, SessionData};
    /// use uuid::Uuid;
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config.clone()).await.unwrap();
    /// let token = Uuid::new_v4();
    /// let session_data = SessionData::new(token, true, &config);
    ///
    /// async {
    ///     let _ = session_store.store_session(&session_data).await.unwrap();
    /// };
    /// ```
    ///
    pub(crate) async fn store_session(&self, session: &SessionData) -> Result<(), SessionError> {
        let _guard = self.session_lock(&session.id).await;
        self.store_session_locked(session).await
    }

    /// LOCAL PATCH: `id` に対応するロックを取る。
    ///
    /// DB への保存と削除はこのロックの下で行い、同じセッションに対する操作が
    /// 追い越し合わないようにする。
    #[inline]
    async fn session_lock(&self, id: &str) -> tokio::sync::MutexGuard<'_, ()> {
        self.locks[self.lock_shard(id)].lock().await
    }

    /// LOCAL PATCH: `id` を割り当てるロックの番号。
    #[inline]
    pub(crate) fn lock_shard(&self, id: &str) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut hasher);
        (hasher.finish() as usize) % self.locks.len()
    }

    /// LOCAL PATCH: 掃除タスク（[`crate::handler::runner`]）からの保存。
    ///
    /// 掃除は「まだ保存されていないかもしれない値を、メモリから降ろす前に同期する」
    /// のが目的なので、対象を先に clone してから await すると、その間に走った
    /// logout の削除を追い越して古い認証情報を書き戻せてしまう。ロックを取ってから
    /// メモリ上の最新の値を読み直し、まだ掃除対象として載っている場合だけ保存する。
    /// 先に logout がメモリから落としていれば、ここでは何もしない。
    ///
    /// 保存したなら `true` を返す。
    pub(crate) async fn store_expired_session(
        &self,
        id: &str,
        current_time: chrono::DateTime<Utc>,
    ) -> Result<bool, SessionError> {
        let _guard = self.session_lock(id).await;

        let session = match self.inner.get(id) {
            Some(session) if session.autoremove < current_time && !session.expired() => {
                session.clone()
            }
            _ => return Ok(false),
        };

        self.store_session_locked(&session).await?;
        Ok(true)
    }

    async fn store_session_locked(&self, session: &SessionData) -> Result<(), SessionError> {
        if let Some(client) = &self.client {
            client
                .store(
                    &session.id,
                    &if let Some(key) = self.config.database.database_key.as_ref() {
                        encrypt::encrypt(&session.id, &serde_json::to_string(session)?, key)
                            .map_err(|e| {
                                SessionError::GenericNotSupportedError(format!(
                                    "Error: {e} Occurred when encrypting a Session.",
                                ))
                            })?
                    } else {
                        serde_json::to_string(session)?
                    },
                    session.expires.timestamp(),
                    &self.config.database.table_name,
                )
                .await?;
        }

        Ok(())
    }

    /// Deletes all sessions in the database.
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config.clone()).await.unwrap();
    ///
    /// async {
    ///     let _ = session_store.clear_store().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn clear_store(&self) -> Result<(), SessionError> {
        if let Some(client) = &self.client {
            client.delete_all(&self.config.database.table_name).await?;
        }

        Ok(())
    }

    /// Deletes all sessions in Memory.
    /// This will also Clear those keys from the filter cache if a persistent database does not exist.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config.clone()).await.unwrap();
    ///
    /// async {
    ///     let _ = session_store.clear().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn clear(&mut self) {
        #[cfg(feature = "key-store")]
        if self.client.is_none() {
            let mut filter = self.filter.write().await;
            self.inner
                .iter()
                .for_each(|value| filter.remove(value.key().as_bytes()));
        }

        self.inner.clear();
    }

    /// Attempts to load check and clear Data.
    ///
    /// If no session is found returns false.
    pub(crate) fn service_session_data(&self, session: &Session<T>) -> bool {
        if let Some(mut inner) = self.inner.get_mut(&session.id) {
            inner.service_clear(
                self.config.memory.memory_lifespan,
                self.config.clear_check_on_load,
            );
            inner.set_request();
            return true;
        }

        false
    }

    #[inline]
    pub(crate) fn renew(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.renew();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn destroy(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.destroy();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn set_longterm(&self, id: String, longterm: bool) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.set_longterm(longterm);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn set_store(&self, id: String, storable: bool) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.set_store(storable);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn update(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.update();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn get<N: serde::de::DeserializeOwned>(&self, id: String, key: &str) -> Option<N> {
        if let Some(instance) = self.inner.get(&id) {
            instance.get(key)
        } else {
            tracing::warn!("Session data unexpectedly missing");
            None
        }
    }

    #[inline]
    pub(crate) fn get_remove<N: serde::de::DeserializeOwned>(
        &self,
        id: String,
        key: &str,
    ) -> Option<N> {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.get_remove(key)
        } else {
            tracing::warn!("Session data unexpectedly missing");
            None
        }
    }

    #[inline]
    pub(crate) fn set(&self, id: String, key: &str, value: impl Serialize) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.set(key, value);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn remove(&self, id: String, key: &str) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.remove(key);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn clear_session_data(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.clear();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn set_session_request(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.set_request();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn remove_session_request(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.remove_request();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn is_session_parallel(&self, id: String) -> bool {
        if let Some(instance) = self.inner.get(&id) {
            instance.is_parallel()
        } else {
            tracing::warn!("Session data unexpectedly missing");
            false
        }
    }

    #[inline]
    pub(crate) async fn count_sessions(&self) -> i64 {
        if self.is_persistent() {
            self.count().await.unwrap_or(0i64)
        } else {
            self.inner.len() as i64
        }
    }

    #[inline]
    pub(crate) fn auto_handles_expiry(&self) -> bool {
        if let Some(client) = &self.client {
            client.auto_handles_expiry()
        } else {
            false
        }
    }

    #[cfg(feature = "advanced")]
    #[cfg_attr(docsrs, doc(cfg(feature = "advanced")))]
    #[inline]
    pub fn verify(&self, id: String) -> Result<(), SessionError> {
        if let Some(instance) = self.inner.get(&id) {
            if instance.expires < Utc::now() {
                Err(SessionError::OldSessionError)
            } else {
                Ok(())
            }
        } else {
            Err(SessionError::NoSessionError)
        }
    }

    #[cfg(feature = "advanced")]
    #[cfg_attr(docsrs, doc(cfg(feature = "advanced")))]
    #[inline]
    pub fn update_database_expires(&self, id: String) -> Result<(), SessionError> {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            if instance.longterm {
                instance.expires = Utc::now() + self.config.max_lifespan;
            } else {
                instance.expires = Utc::now() + self.config.lifespan;
            }

            Ok(())
        } else {
            Err(SessionError::NoSessionError)
        }
    }

    #[cfg(feature = "advanced")]
    #[cfg_attr(docsrs, doc(cfg(feature = "advanced")))]
    #[inline]
    pub fn update_memory_expires(&self, id: String) -> Result<(), SessionError> {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.autoremove = Utc::now() + self.config.memory.memory_lifespan;

            Ok(())
        } else {
            Err(SessionError::NoSessionError)
        }
    }

    #[cfg(feature = "advanced")]
    #[cfg_attr(docsrs, doc(cfg(feature = "advanced")))]
    #[inline]
    pub async fn force_database_update(&self, id: String) -> Result<(), SessionError> {
        let session = if let Some(instance) = self.inner.get(&id) {
            instance.clone()
        } else {
            return Err(SessionError::NoSessionError);
        };

        self.store_session(&session).await
    }

    #[cfg(feature = "advanced")]
    #[cfg_attr(docsrs, doc(cfg(feature = "advanced")))]
    #[inline]
    pub fn memory_remove_session(&self, id: String) -> Result<(), SessionError> {
        let is_parallel = if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.remove_request();
            instance.is_parallel()
        } else {
            return Err(SessionError::NoSessionError);
        };

        if is_parallel {
            let _ = self.inner.remove(&id);
        }

        Ok(())
    }

    #[cfg(feature = "advanced")]
    #[cfg_attr(docsrs, doc(cfg(feature = "advanced")))]
    #[inline]
    pub fn force_memory_remove_session(&self, id: String) -> Result<(), SessionError> {
        if self.inner.remove(&id).is_some() {
            Ok(())
        } else {
            Err(SessionError::NoSessionError)
        }
    }

    #[inline]
    pub(crate) async fn database_remove_session(&self, id: String) -> Result<(), SessionError> {
        let _guard = self.session_lock(&id).await;

        if let Some(client) = &self.client {
            client
                .delete_one_by_id(&id, &self.config.database.table_name)
                .await?;
        }

        Ok(())
    }

    #[cfg(feature = "advanced")]
    #[cfg_attr(docsrs, doc(cfg(feature = "advanced")))]
    #[inline]
    pub async fn remove_stored_database_session(&self, id: String) -> Result<(), SessionError> {
        self.database_remove_session(id).await
    }
}

/// LOCAL PATCH: 掃除タスクの保存が、同じセッションへのリクエスト側の保存・削除と
/// 追い越し合わないことを確かめる。
///
/// 掃除は対象を clone してからストレージ往復を await するので、順序を守らせるものが
/// 無いと logout が消したレコードを書き戻し、失効したはずの Cookie が生き返る。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DatabaseError, DatabasePool};
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::mpsc;

    /// 最初の `store` だけを門で止められるモック。
    ///
    /// 止めている間に別のリクエストを走らせることで、保存の途中に割り込む経路を作る。
    #[derive(Debug, Clone)]
    struct MockPool {
        records: Arc<StdMutex<BTreeMap<String, String>>>,
        /// 呼ばれた操作を流す（`store:<id>` / `delete:<id>`）。
        calls: mpsc::UnboundedSender<String>,
        /// 開くまで `store` を待たせる門。
        gate: Arc<Mutex<()>>,
        /// 門で止める `store` の対象 id。
        pause_id: Arc<StdMutex<Option<String>>>,
    }

    #[async_trait]
    impl DatabasePool for MockPool {
        async fn initiate(&self, _table_name: &str) -> Result<(), DatabaseError> {
            Ok(())
        }

        async fn count(&self, _table_name: &str) -> Result<i64, DatabaseError> {
            Ok(self.records.lock().expect("records").len() as i64)
        }

        async fn store(
            &self,
            id: &str,
            session: &str,
            _expires: i64,
            _table_name: &str,
        ) -> Result<(), DatabaseError> {
            let _ = self.calls.send(format!("store:{id}"));
            let paused = self.pause_id.lock().expect("pause id").as_deref() == Some(id);
            if paused {
                let _gate = self.gate.lock().await;
            }
            self.records
                .lock()
                .expect("records")
                .insert(id.to_string(), session.to_string());
            Ok(())
        }

        async fn load(&self, id: &str, _table_name: &str) -> Result<Option<String>, DatabaseError> {
            Ok(self.records.lock().expect("records").get(id).cloned())
        }

        async fn delete_one_by_id(&self, id: &str, _table_name: &str) -> Result<(), DatabaseError> {
            let _ = self.calls.send(format!("delete:{id}"));
            self.records.lock().expect("records").remove(id);
            Ok(())
        }

        async fn exists(&self, id: &str, _table_name: &str) -> Result<bool, DatabaseError> {
            Ok(self.records.lock().expect("records").contains_key(id))
        }

        async fn delete_by_expiry(&self, _table_name: &str) -> Result<Vec<String>, DatabaseError> {
            Ok(Vec::new())
        }

        async fn delete_all(&self, _table_name: &str) -> Result<(), DatabaseError> {
            self.records.lock().expect("records").clear();
            Ok(())
        }

        async fn get_ids(&self, _table_name: &str) -> Result<Vec<String>, DatabaseError> {
            Ok(self
                .records
                .lock()
                .expect("records")
                .keys()
                .cloned()
                .collect())
        }

        fn auto_handles_expiry(&self) -> bool {
            false
        }
    }

    struct Fixture {
        store: SessionStore<MockPool>,
        pool: MockPool,
        calls: mpsc::UnboundedReceiver<String>,
        gate: Arc<Mutex<()>>,
    }

    async fn fixture() -> Fixture {
        let (calls_tx, calls_rx) = mpsc::unbounded_channel();
        let gate = Arc::new(Mutex::new(()));
        let pool = MockPool {
            records: Arc::new(StdMutex::new(BTreeMap::new())),
            calls: calls_tx,
            gate: gate.clone(),
            pause_id: Arc::new(StdMutex::new(None)),
        };
        let store = SessionStore::new(Some(pool.clone()), SessionConfig::default())
            .await
            .expect("session store");

        Fixture {
            store,
            pool,
            calls: calls_rx,
            gate,
        }
    }

    impl Fixture {
        /// メモリ上にだけある、掃除の対象（`autoremove` が過去）のセッションを置く。
        fn insert_swept(&self, id: &str, value: &str) {
            let mut session = SessionData::new(id.to_string(), true, &self.store.config);
            session.set("user", value);
            session.autoremove = Utc::now() - Duration::try_minutes(5).unwrap_or_default();
            self.store.inner.insert(id.to_string(), session);
        }

        /// `id` への保存を門が開くまで止める。
        fn pause_store(&self, id: &str) {
            *self.pool.pause_id.lock().expect("pause id") = Some(id.to_string());
        }

        fn record(&self, id: &str) -> Option<String> {
            self.pool.records.lock().expect("records").get(id).cloned()
        }

        /// 期待する呼び出しが来るまで待つ。来なければ panic する。
        async fn wait_for_call(&mut self, expected: &str) {
            loop {
                let call =
                    tokio::time::timeout(std::time::Duration::from_secs(5), self.calls.recv())
                        .await
                        .unwrap_or_else(|_| panic!("`{expected}` was never called"))
                        .expect("call channel closed");
                if call == expected {
                    return;
                }
            }
        }
    }

    /// 掃除がロックを取る前に logout が済んでいれば、書き戻しは起きない。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_that_left_memory_is_not_written_back() {
        let fx = fixture().await;
        let id = "left-memory";
        fx.insert_swept(id, "old");

        // logout 相当: メモリから落として DB からも消す。
        fx.store.inner.remove(id);
        fx.store
            .database_remove_session(id.to_string())
            .await
            .expect("remove session");

        // 掃除は対象の一覧を先に作るので、消えた後の id で呼ばれうる。
        let stored = fx
            .store
            .store_expired_session(id, Utc::now())
            .await
            .expect("sweep save");

        assert!(!stored, "メモリに無いセッションを保存してはならない");
        assert_eq!(fx.record(id), None, "logout したセッションが復活している");
    }

    /// 掃除の保存が進行中でも、logout の削除が追い越されない。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_logout_during_the_sweep_wins() {
        let mut fx = fixture().await;
        let id = "logout-race".to_string();
        fx.insert_swept(&id, "old");

        let held = fx.gate.clone().lock_owned().await;
        fx.pause_store(&id);

        let sweep = tokio::spawn({
            let store = fx.store.clone();
            let id = id.clone();
            async move { store.store_expired_session(&id, Utc::now()).await }
        });

        // 掃除が保存に入る = セッションのロックを握ったところまで進める。
        fx.wait_for_call(&format!("store:{id}")).await;

        let logout = tokio::spawn({
            let store = fx.store.clone();
            let id = id.clone();
            async move {
                store.inner.remove(&id);
                store.database_remove_session(id).await
            }
        });

        // 直列化されていれば、logout の削除は保存が終わるまで届かない。
        let leaked = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fx.wait_for_call(&format!("delete:{id}")),
        )
        .await;
        assert!(leaked.is_err(), "logout の削除が掃除の保存を追い越している");

        drop(held);
        sweep.await.expect("sweep task").expect("sweep save");
        logout.await.expect("logout task").expect("logout");

        assert_eq!(fx.record(&id), None, "logout したセッションが復活している");
    }

    /// 掃除の保存が止まっていても、別セッションのリクエストは待たされない。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stalled_sweep_does_not_block_another_session() {
        let mut fx = fixture().await;

        // 同じロックに当たると当然待たされるので、別のロックに載る id を選ぶ。
        let swept = "stalled-sweep".to_string();
        let other = (0..)
            .map(|n| format!("other-{n}"))
            .find(|id| fx.store.lock_shard(id) != fx.store.lock_shard(&swept))
            .expect("another shard");
        fx.insert_swept(&swept, "old");

        let held = fx.gate.clone().lock_owned().await;
        fx.pause_store(&swept);

        let sweep = tokio::spawn({
            let store = fx.store.clone();
            let swept = swept.clone();
            async move { store.store_expired_session(&swept, Utc::now()).await }
        });
        fx.wait_for_call(&format!("store:{swept}")).await;

        let session = SessionData::new(other.clone(), true, &fx.store.config);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fx.store.store_session(&session),
        )
        .await
        .expect("別セッションの保存が掃除に巻き込まれて止まっている")
        .expect("store other session");

        drop(held);
        sweep.await.expect("sweep task").expect("sweep save");
    }
}
