use axum::extract::DefaultBodyLimit;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::handlers::github::MAX_WEBHOOK_BODY_BYTES;

/// `/v1/github` 配下。
///
/// `POST /webhook` だけは公開（セッションも PAT も要らない）で、認証の代わりに
/// `X-Hub-Signature-256` の HMAC を検証する。ボディ上限をこのサブツリー全体に
/// 掛けて、署名検証の前に巨大な本文をメモリに載せないようにする。
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(crate::handlers::github::github_webhook))
        .routes(routes!(crate::handlers::github::get_github_app))
        .routes(routes!(crate::handlers::github::list_installations))
        .routes(routes!(
            crate::handlers::github::list_unclaimed_installations
        ))
        .routes(routes!(crate::handlers::github::claim_installation))
        .routes(routes!(crate::handlers::github::create_setup_state))
        .routes(routes!(
            crate::handlers::github::list_installation_repositories
        ))
        .layer(DefaultBodyLimit::max(MAX_WEBHOOK_BODY_BYTES))
}
