use config::{Config, Environment};
use serde::Deserialize;
use validator::Validate;

#[derive(Clone, Deserialize, Validate)]
pub struct Settings {
    pub database_url: String,
    pub redis_url: String,
    #[serde(default = "default_allow_origin")]
    pub allow_origin: String,
    /// HTTP サーバーの bind アドレス（例: `127.0.0.1:3400`）
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    /// アプリのベース URL（必須。例: `https://app.example.com`）。
    /// OAuth コールバックのリダイレクト先等で使用する。未設定・不正な値では起動しない。
    #[validate(length(min = 1, message = "app_url is required"))]
    #[validate(custom(
        function = "validate_app_url",
        message = "app_url must be a valid http or https base URL"
    ))]
    pub app_url: String,
    /// PAT の HMAC-SHA256 署名に使う秘密鍵。起動時に必須。32バイト以上（256ビット）が必要。
    #[validate(length(
        min = 32,
        message = "PERSONAL_TOKEN_SECRET must be at least 32 characters"
    ))]
    pub personal_token_secret: String,
    /// OAuth アクセストークン暗号化用（AES-256-GCM）。32 文字以上必須。
    #[validate(length(
        min = 32,
        message = "OAUTH_TOKEN_ENCRYPTION_KEY must be at least 32 characters"
    ))]
    pub oauth_token_encryption_key: String,
    /// GitHub OAuth アプリのクライアント ID。
    pub github_client_id: String,
    /// GitHub OAuth アプリのクライアントシークレット。
    pub github_client_secret: String,
    /// GitLab OAuth アプリのクライアント ID。
    pub gitlab_client_id: String,
    /// GitLab OAuth アプリのクライアントシークレット。
    pub gitlab_client_secret: String,
    /// セルフホスト GitLab のベース URL（省略時は gitlab.com）。
    pub gitlab_instance_url: Option<String>,
    /// GitHub App の App ID（PR ステータス連携。未設定時は連携無効）。
    pub github_app_id: Option<u64>,
    /// GitHub App の秘密鍵（PEM。`\n` エスケープ可）。
    pub github_app_private_key_pem: Option<String>,
    /// GitHub App のインストール画面 URL。
    /// 例: `https://github.com/apps/my-vrt-app/installations/new`。
    pub github_app_install_url: Option<String>,
    /// GitHub Webhook の署名検証シークレット。
    pub github_webhook_secret: Option<String>,
    /// GitHub API のベース URL。GitHub Enterprise と統合テスト（wiremock）用の差し替え口。
    /// 未設定なら [`DEFAULT_GITHUB_API_BASE_URL`]。
    pub github_api_base_url: Option<String>,
    /// ストレージバックエンド種別（`local` | `s3`）。
    #[serde(default = "default_storage_backend")]
    pub storage_backend: String,
    /// `local` バックエンドの保存先ディレクトリ。
    #[serde(default = "default_local_upload_dir")]
    pub local_upload_dir: String,
    /// アップロード済み Storybook バンドル（zip）を「Open Storybook」で配信するために
    /// ローカル展開しておくキャッシュディレクトリ。
    ///
    /// ビルドごとに `{storybook_cache_dir}/{build_id}/` へ 1 度だけ展開し、以降はそこから
    /// 静的配信する。バンドルは 1 ビルドにつき不変なので中身も不変。
    /// MVP では自動退避（eviction）は行わない（ディスクが逼迫したら手動でこのディレクトリを消す）。
    #[serde(default = "default_storybook_cache_dir")]
    pub storybook_cache_dir: String,
    /// S3 互換バックエンド設定（`storage_backend = "s3"` のときのみ必須）。
    pub s3_endpoint: Option<String>,
    pub s3_bucket: Option<String>,
    pub s3_region: Option<String>,
    pub s3_access_key_id: Option<String>,
    pub s3_secret_access_key: Option<String>,
    pub s3_public_base_url: Option<String>,
    pub s3_force_path_style: Option<bool>,
    /// ビルド自動プルーニングの最低保持日数（env `STORAGE_MIN_RETENTION_DAYS`、既定 0）。
    ///
    /// 作成からこの日数が経っていないビルドは `build_retention_limit` を超過していても
    /// 削除しない。Wasabi のように削除済みオブジェクトへも最低保存期間（90 日）分を
    /// 課金するストレージでは、期間内に消しても節約にならないため 90 を設定する。
    #[serde(default)]
    pub storage_min_retention_days: u32,
    /// ヘッドレス Chromium の実行ファイルパス（env `CHROMIUM_PATH`）。
    ///
    /// 未設定なら Storybook レンダリング機能そのものが無効になり、
    /// `mode = storybook` のビルド作成は 400 で拒否される。
    pub chromium_path: Option<String>,
    /// API プロセス内で RenderBuild worker を起動するか。
    ///
    /// 既存デプロイとの互換性のため既定は true。独立した `vrt-runner` を使う環境では
    /// API に `RENDER_WORKER_ENABLED=false` を設定し、同じキューを二重に消費しないようにする。
    #[serde(default = "default_render_worker_enabled")]
    pub render_worker_enabled: bool,
    /// Storybook ビルドの受付可否を Chromium の有無から切り離す上書き設定。
    ///
    /// 未指定なら従来どおり `CHROMIUM_PATH` の有無で決まる。API から runner を分離し、
    /// API イメージに Chromium を入れない場合は `STORYBOOK_RENDER_ENABLED=true` を指定する。
    #[serde(default, rename = "storybook_render_enabled")]
    pub storybook_render_enabled_override: Option<bool>,
    /// e2e テスト専用のログイン口 `POST /v1/auth/test-login` を開くフラグ。
    ///
    /// **本番では絶対に有効にしないこと。** 有効にすると、認証情報なしで任意の
    /// ユーザー名のセッションを発行できる = 実質的な認証バイパスになる。
    /// 保険として、release ビルドではこのフラグが立っていると
    /// [`load_settings`] が起動を拒否する。
    #[serde(default)]
    pub test_login_enabled: bool,
}

/// GitHub API の既定ベース URL。
pub const DEFAULT_GITHUB_API_BASE_URL: &str = "https://api.github.com";

impl Settings {
    /// GitHub App が設定されているか（App ID + 秘密鍵の両方が必要）。
    pub fn github_app_enabled(&self) -> bool {
        self.github_app_id.is_some() && self.github_app_private_key_pem.is_some()
    }

    /// Storybook レンダリング（`mode = storybook`）を受け付けるか。
    /// 上書きが無ければ、後方互換のため Chromium の設定有無から決める。
    pub fn storybook_render_enabled(&self) -> bool {
        self.storybook_render_enabled_override
            .unwrap_or_else(|| self.chromium_configured())
    }

    /// このプロセスが Chromium を実行できるか。
    pub fn chromium_configured(&self) -> bool {
        self.chromium_path
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty())
    }

    /// GitHub API のベース URL（末尾スラッシュを落とした形）。
    pub fn github_api_base_url(&self) -> String {
        self.github_api_base_url
            .as_deref()
            .unwrap_or(DEFAULT_GITHUB_API_BASE_URL)
            .trim_end_matches('/')
            .to_string()
    }
}

fn default_allow_origin() -> String {
    "http://localhost:3000".to_string()
}

fn default_listen_addr() -> String {
    "0.0.0.0:3400".to_string()
}

fn default_storage_backend() -> String {
    "local".to_string()
}

fn default_local_upload_dir() -> String {
    "./uploads".to_string()
}

fn default_storybook_cache_dir() -> String {
    "./storybook-cache".to_string()
}

fn default_render_worker_enabled() -> bool {
    true
}

pub fn load_settings() -> Result<Settings, anyhow::Error> {
    dotenvy::dotenv().ok();
    let config = Config::builder()
        .add_source(Environment::default())
        .build()?;

    let mut settings: Settings = config
        .try_deserialize()
        .map_err(|e| anyhow::anyhow!("failed to deserialize settings: {e}"))?;

    // PEM を環境変数で渡す場合の `\n` エスケープを実改行へ戻す。
    if let Some(pem) = settings.github_app_private_key_pem.as_mut() {
        *pem = pem.replace("\\n", "\n");
    }

    settings
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid settings: {e}"))?;

    // 認証バイパスを配布バイナリに混入させないための保険。
    // 開発ビルド（debug_assertions あり）でしか有効にできない。
    if settings.test_login_enabled && !cfg!(debug_assertions) {
        anyhow::bail!(
            "TEST_LOGIN_ENABLED is only allowed in debug builds; refusing to start a release build with the test login endpoint enabled"
        );
    }

    if settings.render_worker_enabled
        && settings.storybook_render_enabled()
        && !settings.chromium_configured()
    {
        anyhow::bail!(
            "RENDER_WORKER_ENABLED=true requires CHROMIUM_PATH when Storybook rendering is enabled"
        );
    }

    Ok(settings)
}

/// 絶対 URL の http(s) ベースのみ許可（`http:/host` のような scheme 直後1スラッシュは拒否）。
fn validate_app_url(raw: &str) -> Result<(), validator::ValidationError> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(validator::ValidationError::new("required"));
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(validator::ValidationError::new("http_or_https"));
    }

    let parsed = url::Url::parse(url).map_err(|_| validator::ValidationError::new("url"))?;

    if parsed.cannot_be_a_base() {
        return Err(validator::ValidationError::new("not_absolute"));
    }

    let Some(host) = parsed.host_str() else {
        return Err(validator::ValidationError::new("host"));
    };

    if host.is_empty() {
        return Err(validator::ValidationError::new("host"));
    }

    let after_scheme = url
        .strip_prefix(parsed.scheme())
        .and_then(|s| s.strip_prefix(':'))
        .unwrap_or("");
    if !after_scheme.starts_with("//") {
        return Err(validator::ValidationError::new("authority"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_settings(app_url: &str) -> Settings {
        Settings {
            database_url: String::new(),
            redis_url: String::new(),
            allow_origin: String::new(),
            listen_addr: default_listen_addr(),
            app_url: app_url.to_string(),
            personal_token_secret: "a".repeat(32),
            oauth_token_encryption_key: "b".repeat(32),
            github_client_id: "github-client-id".into(),
            github_client_secret: "github-client-secret".into(),
            gitlab_client_id: "gitlab-client-id".into(),
            gitlab_client_secret: "gitlab-client-secret".into(),
            gitlab_instance_url: None,
            github_app_id: None,
            github_app_private_key_pem: None,
            github_app_install_url: None,
            github_webhook_secret: None,
            github_api_base_url: None,
            storage_backend: default_storage_backend(),
            local_upload_dir: default_local_upload_dir(),
            storybook_cache_dir: default_storybook_cache_dir(),
            s3_endpoint: None,
            s3_bucket: None,
            s3_region: None,
            s3_access_key_id: None,
            s3_secret_access_key: None,
            s3_public_base_url: None,
            s3_force_path_style: None,
            storage_min_retention_days: 0,
            chromium_path: None,
            render_worker_enabled: default_render_worker_enabled(),
            storybook_render_enabled_override: None,
            test_login_enabled: false,
        }
    }

    fn check(url: &str) -> bool {
        base_settings(url).validate().is_ok()
    }

    #[test]
    fn accepts_valid_base_urls() {
        assert!(check("http://localhost:3000"));
        assert!(check("https://app.example.com"));
    }

    #[test]
    fn rejects_single_slash_after_scheme() {
        assert!(!check("http:/localhost:3000"));
        assert!(!check("https:/example.com"));
    }

    #[test]
    fn rejects_missing_slashes() {
        assert!(!check("http:localhost:3000"));
    }

    #[test]
    fn storybook_rendering_can_be_delegated_without_local_chromium() {
        let mut settings = base_settings("https://app.example.com");
        assert!(!settings.storybook_render_enabled());

        settings.storybook_render_enabled_override = Some(true);
        settings.render_worker_enabled = false;

        assert!(settings.storybook_render_enabled());
        assert!(!settings.chromium_configured());
    }

    #[test]
    fn chromium_keeps_the_legacy_rendering_default_enabled() {
        let mut settings = base_settings("https://app.example.com");
        settings.chromium_path = Some("/usr/bin/chromium".into());

        assert!(settings.storybook_render_enabled());
        assert!(settings.render_worker_enabled);
    }
}
