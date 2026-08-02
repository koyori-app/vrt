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

/// 作成するビルドの座標。
///
/// `mode` はサーバー側 `BuildMode` の serde 表現（`screenshots` / `storybook`）。
/// `storybook` はサーバーがレンダリングし、`screenshots` は CI が PNG を送る。
pub struct NewBuild<'a> {
    pub tenant_slug: &'a str,
    pub project_slug: &'a str,
    pub branch: &'a str,
    pub commit_sha: &'a str,
    pub commit_message: Option<&'a str>,
    pub pull_request_number: Option<i32>,
    pub mode: &'a str,
}

/// finalize のリクエストボディ（payload::builds::FinalizeBuildRequest に一致）。
#[derive(Debug, Serialize)]
struct FinalizeBody<'a> {
    /// `None` は「全ストーリー撮影」（サーバーは省略・null を同義に扱う）。
    #[serde(skip_serializing_if = "Option::is_none")]
    only_story_ids: Option<Vec<String>>,
    /// 差分計画の起点にした baseline のコミット SHA。サーバーはビルドに
    /// 固定された baseline と照合し、ずれていれば finalize を拒否する。
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_baseline_commit_sha: Option<&'a str>,
}

/// capture plan 添付のリクエストボディ（payload::builds::AttachCapturePlanRequest に一致）。
#[derive(Debug, Serialize)]
struct AttachPlanBody<'a> {
    selected_names: &'a [String],
    manifest_names: &'a [String],
    baseline_commit_sha: &'a str,
}

/// BuildResponse のうち CLI が使うフィールドだけ受ける。
///
/// `status` はサーバー側 enum に密結合しないよう文字列で受ける。
#[derive(Debug, Clone, Deserialize)]
pub struct BuildResponse {
    pub id: String,
    /// プロジェクト内で連番のビルド番号（`--json` 出力の build_number）。
    pub number: i64,
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
    pub content_hash_skipped_count: i32,
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

    /// ビルドを作成する。
    ///
    /// レスポンスの `baseline_commit_sha` は作成時にだけ載る。`screenshots` モードで
    /// 撮影前に差分の起点を知りたいときも、この経路で受け取る。
    pub async fn create_build(&self, new: &NewBuild<'_>) -> Result<BuildResponse> {
        let url = format!(
            "{}/v1/ci/projects/{}/{}/builds",
            self.base_url, new.tenant_slug, new.project_slug
        );
        let body = CreateBuildBody {
            branch: new.branch,
            commit_sha: new.commit_sha,
            commit_message: new.commit_message,
            pull_request_number: new.pull_request_number,
            mode: new.mode,
        };
        let resp = self
            .bearer(self.http.post(&url))
            .json(&body)
            .send()
            .await
            .context("create build request failed")?;
        Self::read_json(resp, "create build").await
    }

    /// 部分アップロード計画（capture plan）をビルドへ固定する。
    ///
    /// 撮影を始める前に呼ぶこと。サーバーは計画の起点 `baseline_commit_sha` が
    /// いまも最新の baseline であることを確認してから比較対象を固定する。
    /// baseline が動いていた場合は 409 が返るので、計画を作り直す。
    pub async fn attach_plan(
        &self,
        build_id: &str,
        selected_names: &[String],
        manifest_names: &[String],
        baseline_commit_sha: &str,
    ) -> Result<BuildResponse> {
        let url = format!("{}/v1/ci/builds/{}/plan", self.base_url, build_id);
        let resp = self
            .bearer(self.http.post(&url))
            .json(&AttachPlanBody {
                selected_names,
                manifest_names,
                baseline_commit_sha,
            })
            .send()
            .await
            .context("attach capture plan request failed")?;
        Self::read_json(resp, "attach capture plan").await
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
    /// 差分撮影のときは、計画の起点にした baseline のコミット SHA を
    /// `expected_baseline_commit_sha` として添え、サーバー側の固定値と照合させる。
    pub async fn finalize(
        &self,
        build_id: &str,
        only_story_ids: Option<Vec<String>>,
        expected_baseline_commit_sha: Option<&str>,
    ) -> Result<BuildResponse> {
        let url = format!("{}/v1/ci/builds/{}/finalize", self.base_url, build_id);
        let mut req = self.bearer(self.http.post(&url));
        // `expected_baseline_commit_sha` は `only_story_ids` が無くても単独で
        // 送る。以前は `only_story_ids: Some` のときしかボディを付けず、
        // expected だけを渡した呼び出しが黙って捨てられていた——サーバーの
        // 「screenshots モード + 計画あり + expected のみ」の照合にこの
        // クライアントから到達できず、照合したつもりで送られていない
        // footgun になる。
        if only_story_ids.is_some() || expected_baseline_commit_sha.is_some() {
            req = req.json(&FinalizeBody {
                only_story_ids,
                expected_baseline_commit_sha,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// expected 単独のボディが only_story_ids 無しで送れる形になっていること。
    /// 修正前は only_story_ids が必須フィールドの Vec で、expected 単独の
    /// リクエストは構造的に組めなかった（呼び出し側で黙って捨てていた）。
    #[test]
    fn finalize_body_serializes_expected_without_story_ids() {
        let body = FinalizeBody {
            only_story_ids: None,
            expected_baseline_commit_sha: Some("abc123"),
        };
        let json = serde_json::to_value(&body).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({ "expected_baseline_commit_sha": "abc123" })
        );
    }
}
