//! 「Open Storybook」= アップロード済み Storybook バンドルの対話配信の統合テスト。
//!
//! 本物の Postgres / Valkey（testcontainers）+ ローカルストレージで、次を確認する。
//!
//! - storybook モードのビルドにバンドルを上げると、`/v1/builds/{id}/storybook/…` から
//!   index.html / iframe.html / ネストしたアセットが 200 で配信される
//! - パストラバーサル（`..%2f…`・絶対パス）は 404 に落ちる
//! - screenshots モードのビルドや未認証・非メンバーは配信されない
//!
//! Chromium は不要（撮影はしない）。ビルドはサービス層で直接 storybook モードで作り、
//! バンドルは CI のアップロードエンドポイント経由で保存する。

mod common;

use common::TestApp;
use entity::builds::BuildMode;
use entity::scopes::Scope;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::io::Write as _;
use uuid::Uuid;

/// storybook v5 の最小 index。
const INDEX_JSON: &str = r#"{
  "v": 5,
  "entries": {
    "demo-box--primary": {
      "type": "story",
      "id": "demo-box--primary",
      "title": "Demo/Box",
      "name": "Primary",
      "importPath": "./src/Box.stories.tsx"
    }
  }
}"#;

const MANAGER_HTML: &str =
    "<!doctype html><html><body id=\"manager\">storybook manager</body></html>";
const IFRAME_HTML: &str = "<!doctype html><html><body id=\"preview\">iframe</body></html>";
const MANAGER_JS: &str = "console.log('manager runtime');";

/// アーカイブ直下に index.json を置く形の storybook-static 相当を zip に固める。
fn bundle_zip() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in [
            ("index.json", INDEX_JSON),
            ("index.html", MANAGER_HTML),
            ("iframe.html", IFRAME_HTML),
            ("assets/manager.js", MANAGER_JS),
        ] {
            writer.start_file(name, options).expect("start zip entry");
            writer
                .write_all(contents.as_bytes())
                .expect("write zip entry");
        }
        writer.finish().expect("finish zip");
    }
    buf.into_inner()
}

struct Fixture {
    app: TestApp,
    project_id: Uuid,
    token: String,
}

async fn setup() -> Fixture {
    let app = TestApp::new().await;
    let user = app.login_as_new_user().await;

    let suffix = &Uuid::new_v4().to_string()[..8];
    let tenant_slug = format!("sb-{suffix}");
    let project_slug = format!("ui-{suffix}");

    let res = app
        .post_json(
            "/v1/tenants",
            json!({ "name": "SB Co", "slug": tenant_slug }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "create tenant");
    let tenant: Value = res.json().await.expect("tenant json");
    let tenant_id = tenant["id"].as_str().expect("tenant id").to_string();

    let res = app
        .post_json(
            &format!("/v1/tenants/{tenant_id}/projects"),
            json!({ "name": "UI", "slug": project_slug }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "create project");
    let project: Value = res.json().await.expect("project json");
    let project_id: Uuid = project["id"].as_str().expect("project id").parse().unwrap();

    let (token, _) = app
        .insert_personal_token(user.id, vec![Scope::WriteBuild, Scope::ReadBuild])
        .await;

    Fixture {
        app,
        project_id,
        token,
    }
}

impl Fixture {
    /// 指定モードのビルドをサービス層で直接作る（storybook 作成の Chromium ゲートを避ける）。
    async fn create_build(&self, mode: BuildMode, sha: &str) -> Uuid {
        let build = service::builds::create_build(
            &self.app.state.db,
            self.project_id,
            "main".to_string(),
            sha.to_string(),
            None,
            None,
            mode,
        )
        .await
        .expect("create build");
        build.id
    }

    async fn upload_bundle(&self, build_id: Uuid) {
        let res = self
            .app
            .upload_storybook_bundle(build_id, &self.token, bundle_zip())
            .await;
        assert_eq!(res.status(), StatusCode::CREATED, "upload bundle");
    }
}

#[tokio::test]
async fn serves_uploaded_storybook_over_the_build_endpoint() {
    let fx = setup().await;
    let build_id = fx.create_build(BuildMode::Storybook, "sha-serve").await;
    fx.upload_bundle(build_id).await;

    // index.html（明示） — セッション Cookie 認証
    let res = fx
        .app
        .get(&format!("/v1/builds/{build_id}/storybook/index.html"))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "index.html 200");
    assert!(
        res.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/html"),
        "index.html is html"
    );
    let body = res.text().await.expect("body");
    assert!(body.contains("storybook manager"), "manager html served");

    // ルート（`/storybook/`）は index.html にフォールバックする
    let res = fx
        .app
        .get(&format!("/v1/builds/{build_id}/storybook/"))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "index root 200");
    assert!(
        res.text()
            .await
            .expect("body")
            .contains("storybook manager"),
        "root falls back to index.html"
    );

    // iframe.html
    let res = fx
        .app
        .get(&format!("/v1/builds/{build_id}/storybook/iframe.html"))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "iframe.html 200");

    // ネストしたアセット + Content-Type
    let res = fx
        .app
        .get(&format!(
            "/v1/builds/{build_id}/storybook/assets/manager.js"
        ))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "asset 200");
    assert!(
        res.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/javascript"),
        "js content-type"
    );
    assert!(
        res.text().await.expect("body").contains("manager runtime"),
        "js body served"
    );

    // PAT（read:build）でも配信される
    let res = fx
        .app
        .get_with_bearer(
            &format!("/v1/builds/{build_id}/storybook/index.html"),
            &fx.token,
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "PAT read:build 200");
}

#[tokio::test]
async fn rejects_traversal_attempts() {
    let fx = setup().await;
    let build_id = fx.create_build(BuildMode::Storybook, "sha-trav").await;
    fx.upload_bundle(build_id).await;

    for bad in [
        "..%2f..%2f..%2fetc%2fpasswd",
        "%2fetc%2fpasswd",
        "assets%2f..%2f..%2findex.json",
        "nope.js",
    ] {
        let res = fx
            .app
            .get(&format!("/v1/builds/{build_id}/storybook/{bad}"))
            .await;
        assert!(
            matches!(
                res.status(),
                StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
            ),
            "traversal/missing {bad} must be 400/404, got {}",
            res.status()
        );
    }
}

#[tokio::test]
async fn screenshots_mode_build_has_no_storybook() {
    let fx = setup().await;
    let build_id = fx.create_build(BuildMode::Screenshots, "sha-shots").await;

    let res = fx
        .app
        .get(&format!("/v1/builds/{build_id}/storybook/index.html"))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "screenshots-mode build has no storybook"
    );

    // バンドル未アップロードの storybook ビルドも 404。
    let pending = fx.create_build(BuildMode::Storybook, "sha-pending").await;
    let res = fx
        .app
        .get(&format!("/v1/builds/{pending}/storybook/index.html"))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "storybook build without an uploaded bundle is 404"
    );
}

#[tokio::test]
async fn unauthenticated_and_non_member_are_denied() {
    let fx = setup().await;
    let build_id = fx.create_build(BuildMode::Storybook, "sha-auth").await;
    fx.upload_bundle(build_id).await;

    // 未認証（Cookie も Bearer も無い）
    let anon = reqwest::Client::builder().build().expect("anon client");
    let res = anon
        .get(format!(
            "{}/v1/builds/{build_id}/storybook/index.html",
            fx.app.base_url()
        ))
        .send()
        .await
        .expect("anon get");
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated is 401"
    );

    // 非メンバー（read:build は持つが、このテナントの一員ではない）
    let outsider = fx.app.insert_user().await;
    let (outsider_token, _) = fx
        .app
        .insert_personal_token(outsider.id, vec![Scope::ReadBuild])
        .await;
    let res = fx
        .app
        .get_with_bearer(
            &format!("/v1/builds/{build_id}/storybook/index.html"),
            &outsider_token,
        )
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "non-member is 403");
}
