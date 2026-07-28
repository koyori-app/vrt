//! OAuth プロバイダーのレジストリ。
//!
//! auth-core は「プロトコル部品」しか提供しないため、どのプロバイダーを有効にするか・
//! 資格情報をどこから読むか・コールバック URL をどう組み立てるかはアプリ側の責務になる。
//! ここでルート引数（`/v1/auth/{provider}/...`）から実装を引けるようにまとめる。

use std::collections::HashMap;
use std::sync::Arc;

use auth_core::provider::{OAuthProvider, ProviderConfig};
use auth_core_github::GithubProvider;
use auth_core_gitlab::{GitlabProvider, GitlabSelfHostedProvider};
use common::settings::Settings;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::oauth_state::RedisStateStore;

/// state に載せるアプリ固有のペイロード。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthStatePayload {
    /// state を発行したプロバイダー（コールバックのプロバイダーと突き合わせる）。
    pub provider: String,
    pub code_verifier: String,
    /// ログイン後に戻るフロントの相対パス。
    pub redirect_to: String,
}

#[derive(Debug, Error)]
pub enum OAuthConfigError {
    #[error("unknown oauth provider")]
    UnknownProvider,
    #[error("oauth provider is not configured")]
    NotConfigured,
}

pub struct OAuthProviderEntry {
    pub provider: Arc<dyn OAuthProvider>,
    /// クライアント ID / シークレット。未設定のプロバイダーは `None`。
    pub config: Option<ProviderConfig>,
}

/// 設定済みプロバイダー + Redis の state ストア + 共有 HTTP クライアント。
pub struct OAuthRegistry {
    entries: HashMap<String, OAuthProviderEntry>,
    pub http: reqwest::Client,
    pub state_store: RedisStateStore,
    /// フロント（= アプリ）のベース URL。リダイレクト先の同一 origin 検査に使う。
    pub app_url: String,
}

/// ログイン後の既定リダイレクト先。
pub const DEFAULT_REDIRECT_PATH: &str = "/";

/// GitHub API は User-Agent を要求する。
const USER_AGENT: &str = "vrt";

impl OAuthRegistry {
    pub fn from_settings(
        settings: &Settings,
        redis: common::cache::redis::RedisConnection,
        http: reqwest::Client,
    ) -> Result<Self, anyhow::Error> {
        let mut entries: HashMap<String, OAuthProviderEntry> = HashMap::new();

        entries.insert(
            auth_core_github::SLUG.to_string(),
            OAuthProviderEntry {
                provider: Arc::new(GithubProvider::new(USER_AGENT)),
                config: credentials(&settings.github_client_id, &settings.github_client_secret),
            },
        );

        let gitlab_credentials =
            credentials(&settings.gitlab_client_id, &settings.gitlab_client_secret);

        entries.insert(
            auth_core_gitlab::SLUG.to_string(),
            OAuthProviderEntry {
                provider: Arc::new(GitlabProvider),
                config: gitlab_credentials.clone(),
            },
        );

        // セルフホスト GitLab は instance_url が設定されているときだけ有効。
        // URL の検証（SSRF 対策）は auth-core 側の url_guard が行う。
        if let Some(instance_url) = settings
            .gitlab_instance_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            entries.insert(
                auth_core_gitlab::SELF_HOSTED_SLUG.to_string(),
                OAuthProviderEntry {
                    provider: Arc::new(GitlabSelfHostedProvider::new(instance_url)?),
                    config: gitlab_credentials,
                },
            );
        }

        Ok(Self {
            entries,
            http,
            state_store: RedisStateStore::new(redis),
            app_url: settings.app_url.trim_end_matches('/').to_string(),
        })
    }

    /// ルート引数からプロバイダー実装と資格情報を引く。
    pub fn resolve(
        &self,
        key: &str,
    ) -> Result<(&dyn OAuthProvider, &ProviderConfig), OAuthConfigError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(OAuthConfigError::UnknownProvider)?;
        let config = entry
            .config
            .as_ref()
            .ok_or(OAuthConfigError::NotConfigured)?;
        Ok((entry.provider.as_ref(), config))
    }

    /// OAuth のコールバック URL。フロント（同一 origin）の `/api` プロキシ越しに
    /// 受けることで、セッション Cookie が first-party のまま維持される。
    pub fn callback_url(&self, key: &str) -> String {
        format!("{}/api/v1/auth/{key}/callback", self.app_url)
    }
}

fn credentials(client_id: &str, client_secret: &str) -> Option<ProviderConfig> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return None;
    }
    Some(ProviderConfig {
        client_id: client_id.to_string(),
        client_secret: client_secret.trim().to_string(),
    })
}
