//! 表示言語のユーザー設定（`PATCH /v1/users/me`）。
//!
//! 取り決め:
//! - 新規ユーザーは**未設定**（`language: null`）。画面はブラウザの言語に従う
//! - `en` / `ja` だけを受け付け、それ以外はボディの deserialize で 422
//! - `null` を明示すると未設定へ戻せる（＝ブラウザ判定へ戻す道が要る）
//! - フィールドを省略したボディは据え置き（プロジェクト設定の PATCH と同じ約束）
//! - 本人のセッション専用。PAT からは変えられない（403）、未ログインは 401

mod common;

use common::{TestApp, is_redirect};
use reqwest::StatusCode;
use serde_json::json;

async fn me_language(app: &TestApp) -> serde_json::Value {
    let response = app.get("/v1/users/me").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("me json");
    body["language"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn language_starts_unset_and_survives_a_round_trip() {
    let app = TestApp::new().await;
    assert!(is_redirect(app.login_via_oauth().await.status()));

    // OAuth で作られたばかりのユーザーは未設定。
    assert_eq!(me_language(&app).await, json!(null));

    let updated = app
        .patch_json("/v1/users/me", json!({ "language": "ja" }))
        .await;
    assert_eq!(updated.status(), StatusCode::OK);
    let body: serde_json::Value = updated.json().await.expect("update json");
    assert_eq!(body["language"].as_str(), Some("ja"));

    // 応答だけでなく、次に読んだプロフィールにも残る。
    assert_eq!(me_language(&app).await, json!("ja"));

    // フィールドを省略した PATCH は据え置き——空ボディで設定が消えない。
    let untouched = app.patch_json("/v1/users/me", json!({})).await;
    assert_eq!(untouched.status(), StatusCode::OK);
    assert_eq!(me_language(&app).await, json!("ja"));

    // 明示的な null で「ブラウザに従う」へ戻せる。
    let cleared = app
        .patch_json("/v1/users/me", json!({ "language": null }))
        .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(me_language(&app).await, json!(null));
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_language_tags_are_rejected() {
    let app = TestApp::new().await;
    assert!(is_redirect(app.login_via_oauth().await.status()));

    // 対応外の言語タグ。`ja-JP` や `JA` も通さない——DB に入る値を
    // `en` / `ja` の 2 つに閉じておき、読む側の正規化を不要にする。
    for tag in ["fr", "ja-JP", "JA", ""] {
        let response = app
            .patch_json("/v1/users/me", json!({ "language": tag }))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported language tag {tag:?} must be rejected"
        );
    }

    // 文字列ですらない値は JSON の型エラーとして 400 で弾かれる
    // （422 とは経路が違うだけで、どちらも受け付けない側）。
    let wrong_type = app
        .patch_json("/v1/users/me", json!({ "language": 1 }))
        .await;
    assert!(
        wrong_type.status().is_client_error(),
        "a non-string language must be rejected, got {}",
        wrong_type.status()
    );

    // 弾かれた後も設定は変わっていない。
    assert_eq!(me_language(&app).await, json!(null));
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_the_language_requires_the_owning_session() {
    let app = TestApp::new().await;

    // 未ログインは 401。
    let anonymous = app
        .patch_json("/v1/users/me", json!({ "language": "ja" }))
        .await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    assert!(is_redirect(app.login_via_oauth().await.status()));

    // PAT は参照はできるが変更はできない（CI 用トークンで画面設定は触らせない）。
    let issued = app
        .post_json(
            "/v1/personal_tokens",
            json!({ "name": "ci", "scopes": ["read:project"] }),
        )
        .await;
    assert_eq!(issued.status(), StatusCode::CREATED);
    let body: serde_json::Value = issued.json().await.expect("token json");
    let token = body["token"].as_str().expect("raw token");

    let with_pat = app.get_with_bearer("/v1/users/me", token).await;
    assert_eq!(with_pat.status(), StatusCode::OK);

    let rejected = app
        .patch_json_with_bearer("/v1/users/me", token, json!({ "language": "ja" }))
        .await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert_eq!(me_language(&app).await, json!(null));
}
