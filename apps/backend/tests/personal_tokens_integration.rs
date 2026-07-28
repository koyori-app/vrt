//! パーソナルアクセストークン (PAT) の発行と Bearer 認証。
//!
//! スコープの割り当て（このフェーズの取り決め）:
//! - `GET /v1/users/me` — 認証は必要だがスコープ要求なし（トークンの疎通確認に使える）
//! - `GET /v1/ci/ping`  — `write:build` を要求する CI 向けプローブ
//! - `/v1/personal_tokens/*` — セッション専用（PAT からは PAT を発行できない）

mod common;

use common::{TestApp, is_redirect};
use entity::scopes::Scope;
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// セッションでログインし、指定スコープの PAT を発行する。
async fn issue_token(app: &TestApp, scopes: &[&str]) -> (String, Uuid) {
    let response = app
        .post_json(
            "/v1/personal_tokens",
            json!({ "name": "ci", "scopes": scopes }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let body: serde_json::Value = response.json().await.expect("create token json");
    let token = body["token"].as_str().expect("raw token").to_string();
    let id = Uuid::parse_str(body["id"].as_str().expect("token id")).expect("uuid");

    // 平文トークンはこの応答でのみ返る
    assert!(token.starts_with("pat_"));
    assert_eq!(
        body["token_last_four"].as_str().unwrap(),
        &token[token.len() - 4..]
    );
    assert_eq!(body["revoked"].as_bool(), Some(false));
    assert!(body["last_used_at"].is_null());

    (token, id)
}

#[tokio::test(flavor = "multi_thread")]
async fn pat_authenticates_and_updates_last_used_at() {
    let app = TestApp::new().await;
    assert!(is_redirect(app.login_via_oauth().await.status()));

    let (token, token_id) = issue_token(&app, &["read:project", "write:build"]).await;
    assert!(app.personal_token(token_id).await.last_used_at.is_none());

    let me = app.get_with_bearer("/v1/users/me", &token).await;
    assert_eq!(me.status(), StatusCode::OK);
    let body: serde_json::Value = me.json().await.expect("me json");
    assert_eq!(body["username"].as_str().unwrap(), app.provider.username);

    assert!(
        app.personal_token(token_id).await.last_used_at.is_some(),
        "last_used_at must be updated on PAT authentication"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scope_gated_endpoint_requires_write_build() {
    let app = TestApp::new().await;
    assert!(is_redirect(app.login_via_oauth().await.status()));

    let (ci_token, _) = issue_token(&app, &["write:build"]).await;
    let ping = app.get_with_bearer("/v1/ci/ping", &ci_token).await;
    assert_eq!(ping.status(), StatusCode::OK);

    // read:build だけのトークンは write:build を満たさない
    let (readonly_token, _) = issue_token(&app, &["read:build"]).await;
    let rejected = app.get_with_bearer("/v1/ci/ping", &readonly_token).await;
    assert_eq!(
        rejected.status(),
        StatusCode::FORBIDDEN,
        "read:build must not pass a write:build gate"
    );

    // write:build は read:build を含む（スコープの包含関係）
    let user = app
        .find_user_by_username(&app.provider.username)
        .await
        .expect("oauth user");
    let (write_only, _) = app
        .insert_personal_token(user.id, vec![Scope::WriteBuild])
        .await;
    assert_eq!(
        app.get_with_bearer("/v1/users/me", &write_only)
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_token_is_rejected() {
    let app = TestApp::new().await;
    assert!(is_redirect(app.login_via_oauth().await.status()));

    let (token, token_id) = issue_token(&app, &["read:project"]).await;
    assert_eq!(
        app.get_with_bearer("/v1/users/me", &token).await.status(),
        StatusCode::OK
    );

    let revoke = app.delete(&format!("/v1/personal_tokens/{token_id}")).await;
    assert_eq!(revoke.status(), StatusCode::NO_CONTENT);
    assert!(app.personal_token(token_id).await.revoked);

    assert_eq!(
        app.get_with_bearer("/v1/users/me", &token).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get_with_bearer("/v1/ci/ping", &token).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_token_is_rejected() {
    let app = TestApp::new().await;

    let response = app
        .get_with_bearer("/v1/users/me", "pat_definitely-not-a-real-token")
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn token_listing_and_management_is_session_only() {
    let app = TestApp::new().await;
    assert!(is_redirect(app.login_via_oauth().await.status()));

    let (token, token_id) = issue_token(&app, &["read:project"]).await;

    // セッションでは一覧が引ける（平文トークンは含まれない）
    let list = app.get("/v1/personal_tokens").await;
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = list.json().await.expect("list json");
    let items = body.as_array().expect("array");
    assert!(items.iter().any(|item| item["id"] == token_id.to_string()));
    assert!(
        items.iter().all(|item| item.get("token").is_none()),
        "raw tokens must never be listed"
    );

    // PAT では PAT を管理できない
    assert_eq!(
        app.get_with_bearer("/v1/personal_tokens", &token)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let create_with_pat = app
        .client()
        .post(format!("{}/v1/personal_tokens", app.base_url()))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .json(&json!({ "name": "nested", "scopes": ["read:project"] }))
        .send()
        .await
        .expect("post request");
    assert_eq!(create_with_pat.status(), StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_token_requires_authentication_and_scopes() {
    let mut app = TestApp::new().await;
    app.reset_session_client();

    let unauthenticated = app
        .post_json(
            "/v1/personal_tokens",
            json!({ "name": "ci", "scopes": ["read:project"] }),
        )
        .await;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    assert!(is_redirect(app.login_via_oauth().await.status()));

    // スコープ無しは 400
    let empty_scopes = app
        .post_json("/v1/personal_tokens", json!({ "name": "ci", "scopes": [] }))
        .await;
    assert_eq!(empty_scopes.status(), StatusCode::BAD_REQUEST);

    // 未知のスコープ文字列は deserialize で弾かれる
    let bad_scope = app
        .post_json(
            "/v1/personal_tokens",
            json!({ "name": "ci", "scopes": ["admin:everything"] }),
        )
        .await;
    assert_eq!(bad_scope.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
