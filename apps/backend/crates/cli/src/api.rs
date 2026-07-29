//! VRT の CI API クライアント。
//!
//! 認証は `Authorization: Bearer <token>`（ci.rs の想定に一致）。
//! トークンはヘッダにだけ載せ、ログ・エラーメッセージには絶対に出さない。

use anyhow::{Context, Result, bail};
use reqwest::multipart;
use serde::{Deserialize, Serialize};

/// API のベース URL とトークンを持つ薄いクライアント。
pub struct Client {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

/// create_build のリクエストボディ（payload::builds::CreateBuildRequest に一致）。
#[derive(Debug, Serialize)]
struct CreateBuildBody<'a> {
    branch: &'a str,
    commit_sha: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pull_request_number: Option<i32>,
    mode: &'a str,
}

/// finalize のリクエストボディ（payload::builds::FinalizeBuildRequest に一致）。
#[derive(Debug, Serialize)]
struct FinalizeBody {
    only_story_ids: Vec<String>,
}

/// BuildResponse のうち CLI が使うフィールドだけ受ける。
///
/// `status` はサーバー側 enum に密結合しないよう文字列で受ける。
#[derive(Debug, Clone, Deserialize)]
pub struct BuildResponse {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub baseline_commit_sha: Option<String>,
    #[serde(default)]
    pub total_count: i32,
    #[serde(default)]
    pub changed_count: i32,
    #[serde(default)]
    pub added_count: i32,
    #[serde(default)]
    pub removed_count: i32,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// ビルド進捗ログの 1 行（payload::builds::BuildLogEntry に一致）。
#[derive(Debug, Clone, Deserialize)]
pub struct BuildLogEntry {
    pub id: i64,
    pub level: String,
    pub message: String,
}

/// ログの増分取得レスポンス（payload::builds::BuildLogsResponse に一致）。
#[derive(Debug, Clone, Deserialize)]
pub struct BuildLogsResponse {
    pub entries: Vec<BuildLogEntry>,
    pub last_id: i64,
}

impl Client {
    pub fn new(base_url: String, token: String) -> Result<Self> {
        // 末尾スラッシュを一度落として URL 組み立てを安定させる。
        let base_url = base_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            base_url,
            token,
            http,
        })
    }

    fn bearer(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("Authorization", format!("Bearer {}", self.token))
    }

    /// 失敗時に本文を添えてエラーにする（本文にトークンは含まれない）。
    async fn read_json<T: for<'de> Deserialize<'de>>(
        resp: reqwest::Response,
        ctx: &str,
    ) -> Result<T> {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("{ctx} failed: HTTP {status}: {}", body.trim());
        }
        serde_json::from_str(&body).with_context(|| format!("{ctx}: could not parse response body"))
    }

    /// ビルドを作成する（mode=storybook）。
    pub async fn create_build(
        &self,
        tenant_slug: &str,
        project_slug: &str,
        branch: &str,
        commit_sha: &str,
        commit_message: Option<&str>,
        pull_request_number: Option<i32>,
    ) -> Result<BuildResponse> {
        let url = format!(
            "{}/v1/ci/projects/{}/{}/builds",
            self.base_url, tenant_slug, project_slug
        );
        let body = CreateBuildBody {
            branch,
            commit_sha,
            commit_message,
            pull_request_number,
            mode: "storybook",
        };
        let resp = self
            .bearer(self.http.post(&url))
            .json(&body)
            .send()
            .await
            .context("create build request failed")?;
        Self::read_json(resp, "create build").await
    }

    /// Storybook バンドル（zip）を multipart でアップロードする。
    pub async fn upload_storybook(&self, build_id: &str, zip_bytes: Vec<u8>) -> Result<()> {
        let url = format!("{}/v1/ci/builds/{}/storybook", self.base_url, build_id);
        let part = multipart::Part::bytes(zip_bytes)
            .file_name("storybook-static.zip")
            .mime_str("application/zip")
            .context("invalid multipart part")?;
        let form = multipart::Form::new().part("file", part);
        let resp = self
            .bearer(self.http.post(&url))
            .multipart(form)
            .send()
            .await
            .context("storybook upload request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("storybook upload failed: HTTP {status}: {}", body.trim());
        }
        Ok(())
    }

    /// finalize する。`only_story_ids` が `Some` なら差分撮影、`None` なら全撮影。
    ///
    /// 全撮影はボディ無しの POST（ci.rs は空ボディを全撮影として扱う）。
    pub async fn finalize(
        &self,
        build_id: &str,
        only_story_ids: Option<Vec<String>>,
    ) -> Result<BuildResponse> {
        let url = format!("{}/v1/ci/builds/{}/finalize", self.base_url, build_id);
        let mut req = self.bearer(self.http.post(&url));
        if let Some(ids) = only_story_ids {
            req = req.json(&FinalizeBody {
                only_story_ids: ids,
            });
        }
        let resp = req.send().await.context("finalize request failed")?;
        Self::read_json(resp, "finalize").await
    }

    /// ビルドの現在状態を取得する（ポーリング用）。
    pub async fn get_build(&self, build_id: &str) -> Result<BuildResponse> {
        let url = format!("{}/v1/ci/builds/{}", self.base_url, build_id);
        let resp = self
            .bearer(self.http.get(&url))
            .send()
            .await
            .context("get build request failed")?;
        Self::read_json(resp, "get build").await
    }

    /// ビルド進捗ログを `after` カーソルで増分取得する。
    pub async fn get_build_logs(&self, build_id: &str, after: i64) -> Result<BuildLogsResponse> {
        let url = format!(
            "{}/v1/ci/builds/{}/logs?after={}",
            self.base_url, build_id, after
        );
        let resp = self
            .bearer(self.http.get(&url))
            .send()
            .await
            .context("get build logs request failed")?;
        Self::read_json(resp, "get build logs").await
    }
}
