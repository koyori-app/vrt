//! 外部 API 呼び出し用の共有 HTTP クライアント。

use std::time::Duration;

/// コネクションプールを共有するため、アプリ全体で 1 インスタンスを使い回す。
pub fn create_http_client() -> Result<reqwest::Client, anyhow::Error> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("vrt")
        .build()?)
}
