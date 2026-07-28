//! OAuth ログインの往復（wiremock で偽装した GitLab インスタンス）。
//!
//! `GitlabSelfHostedProvider` はインスタンス URL からエンドポイントを組み立てるため、
//! wiremock を指した self-hosted GitLab として往復させるのが実機に最も近い経路になる。
//! `GitlabProvider`（gitlab.com）と `GithubProvider` はエンドポイントが固定で、
//! 差分は「どの URL を叩くか」だけなので、この 1 本でフロー全体を検証する。

mod common;

use common::{TestApp, is_redirect, location_of};
use reqwest::StatusCode;
use serde_json::json;
use service::oauth::{OAuthConfigError, OAuthRegistry};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn login_redirect_carries_state_and_pkce_challenge() {
    let app = TestApp::new().await;

    let response = app.oauth_login(Some("/dashboard")).await;
    assert!(is_redirect(response.status()), "login should redirect");

    let location = location_of(&response);
    let url = url::Url::parse(&location).expect("authorize url");

    // 認可 URL は wiremock インスタンスの /oauth/authorize を指す
    assert!(
        location.starts_with(&format!("{}/oauth/authorize", app.provider.instance_url())),
        "unexpected authorize url: {location}"
    );

    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert!(!query["state"].is_empty(), "state must be present");
    assert!(
        !query["code_challenge"].is_empty(),
        "PKCE challenge must be present"
    );
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], common::TEST_OAUTH_CLIENT_ID);
    assert!(
        query["redirect_uri"].ends_with("/api/v1/auth/gitlab_selfhosted/callback"),
        "unexpected redirect_uri: {}",
        query["redirect_uri"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn callback_creates_user_connection_and_session() {
    let app = TestApp::new().await;

    let callback = app.login_via_oauth().await;
    assert!(
        is_redirect(callback.status()),
        "callback should redirect to the frontend, got {}",
        callback.status()
    );
    assert!(
        callback
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .is_some(),
        "callback must issue a session cookie"
    );
    // 既定のリダイレクト先はフロントのルート（同一 origin）
    assert_eq!(location_of(&callback), format!("{}/", app.base_url()));

    // ユーザーと OAuth 連携が作られている
    let user = app
        .find_user_by_username(&app.provider.username)
        .await
        .expect("oauth user created");
    assert_eq!(user.email.as_deref(), Some(app.provider.email.as_str()));

    let connections = app.connections_for_user(user.id).await;
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0].provider, "gitlab_selfhosted");
    assert_eq!(
        connections[0].provider_user_id,
        app.provider.user_id.to_string()
    );
    // アクセストークンは平文で保存しない（AES-256-GCM で暗号化）
    let stored = connections[0]
        .access_token_enc
        .as_deref()
        .expect("access token stored");
    assert_ne!(stored, common::MOCK_ACCESS_TOKEN);
    assert_eq!(
        service::auth::decrypt_oauth_token(&app.state.settings.oauth_token_encryption_key, stored)
            .expect("decrypt access token"),
        common::MOCK_ACCESS_TOKEN
    );

    // 発行されたセッション Cookie で /v1/users/me が引ける
    let me = app.get("/v1/users/me").await;
    assert_eq!(me.status(), StatusCode::OK);
    let body: serde_json::Value = me.json().await.expect("me json");
    assert_eq!(body["id"].as_str().unwrap(), user.id.to_string());
    assert_eq!(body["username"].as_str().unwrap(), app.provider.username);
}

#[tokio::test(flavor = "multi_thread")]
async fn second_login_reuses_the_existing_user() {
    let app = TestApp::new().await;

    let first = app.login_via_oauth().await;
    assert!(is_redirect(first.status()));
    let user = app
        .find_user_by_username(&app.provider.username)
        .await
        .expect("oauth user created");

    let before = app.count_own_users().await;

    let mut app = app;
    app.reset_session_client();
    let second = app.login_via_oauth().await;
    assert!(is_redirect(second.status()));

    assert_eq!(app.count_own_users().await, before, "no duplicate user");
    assert_eq!(app.connections_for_user(user.id).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn replayed_state_is_rejected() {
    let app = TestApp::new().await;

    let start = app.oauth_login(None).await;
    let state = TestApp::state_from_authorize_url(&location_of(&start));

    let first = app.oauth_callback("mock-auth-code", &state).await;
    assert!(is_redirect(first.status()), "first callback succeeds");

    let users_after_first = app.count_own_users().await;

    // state は GETDEL で使い捨てなので 2 回目は消費できない
    let replay = app.oauth_callback("mock-auth-code", &state).await;
    assert!(
        is_redirect(replay.status()),
        "replay is redirected back to the frontend with an error"
    );
    assert!(
        location_of(&replay).contains("oauth_error=authorization_failed"),
        "replay must carry the oauth error, got {}",
        location_of(&replay)
    );
    assert_eq!(
        app.count_own_users().await,
        users_after_first,
        "replay must not create another user"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn callback_from_a_different_session_is_rejected() {
    let mut app = TestApp::new().await;

    let start = app.oauth_login(None).await;
    let state = TestApp::state_from_authorize_url(&location_of(&start));

    // セッション固定対策: 認可を開始したブラウザと別セッションからは通らない
    app.reset_session_client();
    let response = app.oauth_callback("mock-auth-code", &state).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_provider_returns_404() {
    let app = TestApp::new().await;

    let response = app.get("/v1/auth/bitbucket/login").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn open_redirect_targets_are_rejected() {
    let app = TestApp::new().await;

    let response = app
        .get("/v1/auth/gitlab_selfhosted/login?redirect_to=https%3A%2F%2Fevil.example%2Fphish")
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let protocol_relative = app
        .get("/v1/auth/gitlab_selfhosted/login?redirect_to=%2F%2Fevil.example")
        .await;
    assert_eq!(protocol_relative.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_destroys_the_session() {
    let app = TestApp::new().await;

    assert!(is_redirect(app.login_via_oauth().await.status()));
    assert_eq!(app.get("/v1/users/me").await.status(), StatusCode::OK);

    let logout = app
        .post_json("/v1/auth/logout", serde_json::json!({}))
        .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);

    assert_eq!(
        app.get("/v1/users/me").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// クライアント ID 未設定のプロバイダーは「未知」ではなく「未設定」として扱う
/// （ハンドラでは 400 になる）。
#[tokio::test(flavor = "multi_thread")]
async fn provider_without_client_id_is_not_configured() {
    let app = TestApp::new().await;

    let mut settings = app.state.settings.clone();
    settings.gitlab_client_id = String::new();
    settings.gitlab_client_secret = String::new();
    settings.gitlab_instance_url = None;

    let registry = OAuthRegistry::from_settings(
        &settings,
        app.state.redis_client.clone(),
        service::http::create_http_client().expect("http client"),
    )
    .expect("registry");

    assert!(matches!(
        registry.resolve("gitlab"),
        Err(OAuthConfigError::NotConfigured)
    ));
    assert!(matches!(
        registry.resolve("bitbucket"),
        Err(OAuthConfigError::UnknownProvider)
    ));
    assert!(registry.resolve("github").is_ok());
}

/// テスト専用ログイン口は既定で存在しないこと（本番構成のガード）。
#[tokio::test(flavor = "multi_thread")]
async fn test_login_is_disabled_by_default() {
    let app = TestApp::new().await;
    let res = app
        .post_json("/v1/auth/test-login", json!({ "username": "someone" }))
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // セッションは発行されていない。
    assert_eq!(
        app.get("/v1/users/me").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// `TEST_LOGIN_ENABLED=true` のときだけユーザーを作ってセッションを張る（e2e の前提）。
#[tokio::test(flavor = "multi_thread")]
async fn test_login_creates_user_and_session_when_enabled() {
    let app = TestApp::new_with_test_login().await;
    let username = format!("e2e-{}", &Uuid::new_v4().to_string()[..8]);

    let res = app
        .post_json("/v1/auth/test-login", json!({ "username": username }))
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let me = app.get("/v1/users/me").await;
    assert_eq!(me.status(), StatusCode::OK);
    let body: serde_json::Value = me.json().await.expect("me json");
    assert_eq!(body["username"].as_str(), Some(username.as_str()));

    // 2 回目は同じユーザーを再利用する（冪等）。
    let first_id = body["id"].as_str().expect("id").to_string();
    let res = app
        .post_json("/v1/auth/test-login", json!({ "username": username }))
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let body: serde_json::Value = app.get("/v1/users/me").await.json().await.expect("me json");
    assert_eq!(body["id"].as_str(), Some(first_id.as_str()));

    // 空のユーザー名は 400。
    let res = app
        .post_json("/v1/auth/test-login", json!({ "username": "  " }))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
