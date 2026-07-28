//! storybook モード（サーバーサイドレンダリング）の統合テスト。
//!
//! 本物の Postgres / Valkey（testcontainers）+ ローカルストレージ + apalis ワーカー
//! + 本物のヘッドレス Chromium で、次を一気通貫で確認する。
//!
//! ビルド作成（`mode = storybook`）→ バンドル zip アップロード → finalize →
//! `RenderBuildJob` が全ストーリーを撮影 → `CompareBuildJob` が既存の比較経路を通す。
//!
//! ## フィクスチャ
//!
//! 本物の Storybook をビルドする必要は無い。レンダラが要求するのは
//!
//! - `index.json`（v5 形式。`docs` エントリは撮らない）
//! - `?id=` を読んで `#storybook-root` に何か描く `iframe.html`
//!
//! の 2 つだけなので、手書きの最小バンドルを zip で組み立てる。
//! 描画はストーリー ID ごとに固定色のベタ塗り（フォント・アニメーション無し）なので
//! スクリーンショットは決定的になる。

mod common;

use std::io::Write as _;
use std::time::Duration;

use common::TestApp;
use entity::scopes::Scope;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// ワーカー（レンダリング + 比較）の処理待ちタイムアウト。
/// Chromium の起動ぶん、スクリーンショットモードのテストより長めに取る。
const POLL_TIMEOUT: Duration = Duration::from_secs(120);

/// レンダリングに使うビューポート。既定（1280x720）と違う値にして、
/// プロジェクト設定が実際に効いていることを確認する。
const VIEWPORT_WIDTH: u32 = 320;
const VIEWPORT_HEIGHT: u32 = 240;

// ── フィクスチャ ────────────────────────────────────────────────────────

/// ストーリー 2 件 + docs 1 件の最小 index。
const INDEX_JSON: &str = r#"{
  "v": 5,
  "entries": {
    "demo-box--red": {
      "type": "story",
      "id": "demo-box--red",
      "title": "Demo/Box",
      "name": "Red",
      "importPath": "./src/Box.stories.tsx"
    },
    "demo-box--blue": {
      "type": "story",
      "id": "demo-box--blue",
      "title": "Demo/Box",
      "name": "Blue",
      "importPath": "./src/Box.stories.tsx"
    },
    "demo-box--docs": {
      "type": "docs",
      "id": "demo-box--docs",
      "title": "Demo/Box",
      "name": "Docs",
      "importPath": "./src/Box.mdx"
    }
  }
}"#;

/// `?id=` に対応する色でビューポート全面を塗る `iframe.html`。
///
/// - フォントもアニメーションも使わない（= ピクセルが揺れない）
/// - `RED_OVERRIDE` を差し替えると「変更のあった 2 回目のビルド」を作れる
fn iframe_html(red: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><style>html,body{{margin:0;padding:0;background:#fff}}</style></head>
<body>
<div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var colors = {{ 'demo-box--red': '{red}', 'demo-box--blue': '#0000ff' }};
  var el = document.createElement('div');
  el.style.width = '100vw';
  el.style.height = '100vh';
  el.style.background = colors[id] || '#00ff00';
  document.getElementById('storybook-root').appendChild(el);
</script>
</body>
</html>"#
    )
}

/// `storybook-static/` 相当を zip に固める（アーカイブ直下に index.json を置く形）。
fn bundle_zip(red: &str) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in [
            ("index.json", INDEX_JSON.to_string()),
            ("iframe.html", iframe_html(red)),
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

// ── セットアップ ────────────────────────────────────────────────────────

struct Fixture {
    app: TestApp,
    tenant_slug: String,
    project_slug: String,
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
            json!({ "name": "Storybook Co", "slug": tenant_slug }),
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
    let project_id = project["id"].as_str().expect("project id").to_string();
    assert_eq!(
        project["viewport_width"].as_i64(),
        Some(1280),
        "projects start at the default viewport"
    );

    // 撮影を速くするためにビューポートを小さくする（設定が効くことの確認も兼ねる）。
    let res = app
        .patch_json(
            &format!("/v1/projects/{project_id}"),
            json!({ "viewport_width": VIEWPORT_WIDTH, "viewport_height": VIEWPORT_HEIGHT }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "set project viewport");
    let project: Value = res.json().await.expect("project json");
    assert_eq!(
        project["viewport_width"].as_i64(),
        Some(VIEWPORT_WIDTH as i64)
    );
    assert_eq!(
        project["viewport_height"].as_i64(),
        Some(VIEWPORT_HEIGHT as i64)
    );

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;

    Fixture {
        app,
        tenant_slug,
        project_slug,
        token,
    }
}

impl Fixture {
    async fn create_build(&self, mode: &str, sha: &str) -> reqwest::Response {
        self.app
            .post_json_with_bearer(
                &format!(
                    "/v1/ci/projects/{}/{}/builds",
                    self.tenant_slug, self.project_slug
                ),
                &self.token,
                json!({ "branch": "main", "commit_sha": sha, "mode": mode }),
            )
            .await
    }

    async fn create_storybook_build(&self, sha: &str) -> Value {
        let res = self.create_build("storybook", sha).await;
        assert_eq!(res.status(), StatusCode::CREATED, "create storybook build");
        res.json().await.expect("build json")
    }

    async fn upload_bundle(&self, build_id: Uuid, zip: Vec<u8>) -> reqwest::Response {
        self.app
            .upload_storybook_bundle(build_id, &self.token, zip)
            .await
    }

    async fn finalize(&self, build_id: Uuid) -> reqwest::Response {
        self.app
            .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &self.token)
            .await
    }

    async fn get_build(&self, build_id: Uuid) -> Value {
        let res = self
            .app
            .get_with_bearer(&format!("/v1/ci/builds/{build_id}"), &self.token)
            .await;
        assert_eq!(res.status(), StatusCode::OK, "poll build status");
        res.json().await.expect("build json")
    }

    /// CI 用のポーリングエンドポイントで終端状態になるまで待つ。
    async fn wait_for_terminal(&self, build_id: Uuid) -> Value {
        let deadline = std::time::Instant::now() + POLL_TIMEOUT;
        loop {
            let build = self.get_build(build_id).await;
            let status = build["status"].as_str().unwrap_or_default().to_string();

            if !matches!(status.as_str(), "pending" | "rendering" | "processing") {
                return build;
            }
            if std::time::Instant::now() >= deadline {
                panic!("build {build_id} stuck in {status} after {POLL_TIMEOUT:?}");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// サーバーが撮った screenshots 行を DB から直接読む（一覧 API はまだ無い）。
    async fn screenshots(&self, build_id: Uuid) -> Vec<entity::screenshots::Model> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        entity::screenshots::Entity::find()
            .filter(entity::screenshots::Column::BuildId.eq(build_id))
            .order_by_asc(entity::screenshots::Column::Name)
            .all(&self.app.state.db)
            .await
            .expect("load screenshots")
    }

    async fn comparisons(&self, build_id: Uuid) -> Vec<Value> {
        let res = self
            .app
            .get(&format!("/v1/builds/{build_id}/comparisons"))
            .await;
        assert_eq!(res.status(), StatusCode::OK, "list comparisons");
        let body: Value = res.json().await.expect("comparisons json");
        body["comparisons"]
            .as_array()
            .expect("comparisons array")
            .clone()
    }
}

fn build_id_of(build: &Value) -> Uuid {
    build["id"].as_str().expect("build id").parse().unwrap()
}

/// Chromium が見つからない環境ではレンダリングを伴うテストを飛ばす。
///
/// このリポジトリの開発環境と CI にはシステム chromium か Playwright の
/// キャッシュがあるので、通常はスキップされない。
fn chromium_or_skip(test: &str) -> bool {
    if service::render::discover_chromium().is_some() {
        return true;
    }
    eprintln!(
        "SKIP {test}: no chromium found. \
         Set CHROMIUM_PATH, install chromium, or run `pnpm exec playwright install chromium`."
    );
    false
}

// ── 本体 ────────────────────────────────────────────────────────────────

/// storybook バンドルを投げてから比較結果が出るまでの一気通貫。
#[tokio::test(flavor = "multi_thread")]
async fn storybook_bundle_is_rendered_server_side_and_compared() {
    if !chromium_or_skip("storybook_bundle_is_rendered_server_side_and_compared") {
        return;
    }
    let fx = setup().await;

    // ── ビルド #1: 初回。baseline が無いので全ストーリーが added ─────────
    let build1 = fx.create_storybook_build("sb00001").await;
    assert_eq!(build1["mode"].as_str(), Some("storybook"));
    assert_eq!(build1["status"].as_str(), Some("pending"));
    assert_eq!(build1["storybook_uploaded"].as_bool(), Some(false));
    let build1_id = build_id_of(&build1);

    // バンドル未アップロードで finalize すると 400（レンダリング対象が無い）。
    assert_eq!(
        fx.finalize(build1_id).await.status(),
        StatusCode::BAD_REQUEST,
        "finalize before upload must be rejected"
    );

    // storybook モードのビルドに PNG を直アップロードするのは 409。
    assert_eq!(
        fx.app
            .upload_screenshot(build1_id, &fx.token, "home", vec![0u8; 8])
            .await
            .status(),
        StatusCode::CONFLICT,
        "screenshot upload is not allowed in storybook mode"
    );

    let zip = bundle_zip("#ff0000");
    let expected_size = zip.len() as u64;
    let res = fx.upload_bundle(build1_id, zip.clone()).await;
    assert_eq!(res.status(), StatusCode::CREATED, "upload bundle");
    let uploaded: Value = res.json().await.expect("upload json");
    assert_eq!(uploaded["size_bytes"].as_u64(), Some(expected_size));

    // 1 ビルドにつき 1 本まで。
    assert_eq!(
        fx.upload_bundle(build1_id, zip).await.status(),
        StatusCode::CONFLICT,
        "re-uploading a bundle must be rejected"
    );

    let res = fx.finalize(build1_id).await;
    assert_eq!(res.status(), StatusCode::OK, "finalize storybook build");
    let finalized: Value = res.json().await.expect("finalize json");
    assert_eq!(
        finalized["status"].as_str(),
        Some("rendering"),
        "storybook finalize goes to rendering, not processing"
    );

    let build1 = fx.wait_for_terminal(build1_id).await;
    assert_eq!(
        build1["status"].as_str(),
        Some("changes_detected"),
        "first build reports every story as added (error: {:?})",
        build1["error_message"]
    );
    assert_eq!(
        build1["total_count"].as_i64(),
        Some(2),
        "docs entry skipped"
    );
    assert_eq!(build1["added_count"].as_i64(), Some(2));

    // サーバーが撮ったスクリーンショットは `{title}/{name}` で並ぶ。
    let shots = fx.screenshots(build1_id).await;
    let names: Vec<&str> = shots.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Demo/Box/Blue", "Demo/Box/Red"]);

    for shot in &shots {
        // 撮影サイズはプロジェクトのビューポート設定どおり。
        assert_eq!(
            (shot.width, shot.height),
            (VIEWPORT_WIDTH as i32, VIEWPORT_HEIGHT as i32),
            "screenshot {} uses the project viewport",
            shot.name
        );
        // どのストーリーから来たかを metadata に残している。
        let story_id = shot
            .metadata
            .as_ref()
            .and_then(|m| m.get("story_id"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            story_id.starts_with("demo-box--"),
            "screenshot {} should carry its story id, got {story_id:?}",
            shot.name
        );
    }

    // baseline を作るために force 承認。
    let res = fx
        .app
        .post_json(
            &format!("/v1/builds/{build1_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "force approve build 1");

    // ── ビルド #2: 同じバンドル → 差分ゼロ（レンダリングが決定的なことの証明）
    let build2 = fx.create_storybook_build("sb00002").await;
    let build2_id = build_id_of(&build2);
    assert_eq!(
        fx.upload_bundle(build2_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build2_id).await.status(), StatusCode::OK);

    let build2 = fx.wait_for_terminal(build2_id).await;
    assert_eq!(
        build2["status"].as_str(),
        Some("passed"),
        "identical bundle renders identically (error: {:?})",
        build2["error_message"]
    );
    assert_eq!(build2["unchanged_count"].as_i64(), Some(2));

    // ── ビルド #3: 赤 → 緑に変えたバンドル → 1 枚だけ changed ────────────
    let build3 = fx.create_storybook_build("sb00003").await;
    let build3_id = build_id_of(&build3);
    assert_eq!(
        fx.upload_bundle(build3_id, bundle_zip("#00aa00"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build3_id).await.status(), StatusCode::OK);

    let build3 = fx.wait_for_terminal(build3_id).await;
    assert_eq!(
        build3["status"].as_str(),
        Some("changes_detected"),
        "changing a story's color must be detected (error: {:?})",
        build3["error_message"]
    );
    assert_eq!(build3["changed_count"].as_i64(), Some(1));
    assert_eq!(build3["unchanged_count"].as_i64(), Some(1));

    let cmps = fx.comparisons(build3_id).await;
    let changed: Vec<&str> = cmps
        .iter()
        .filter(|c| c["status"] == "changed")
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert_eq!(changed, vec!["Demo/Box/Red"], "only the red story changed");
}

/// 壊れたバンドルはビルドを `failed` にし、原因が `error_message` に残る。
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_without_an_index_fails_the_build_with_a_reason() {
    let fx = setup().await;

    let build = fx.create_storybook_build("sb0bad1").await;
    let build_id = build_id_of(&build);

    // index.json の無い zip。
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("iframe.html", options).expect("start");
        writer.write_all(b"<html></html>").expect("write");
        writer.finish().expect("finish");
    }

    assert_eq!(
        fx.upload_bundle(build_id, buf.into_inner()).await.status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_id).await.status(), StatusCode::OK);

    let build = fx.wait_for_terminal(build_id).await;
    assert_eq!(build["status"].as_str(), Some("failed"));
    let message = build["error_message"].as_str().unwrap_or_default();
    assert!(
        message.contains("index.json"),
        "error message should name the problem, got {message:?}"
    );
}

/// zip ですらないファイルはアップロード時点で弾く（レンダリングまで行かせない）。
#[tokio::test(flavor = "multi_thread")]
async fn non_zip_uploads_are_rejected() {
    let fx = setup().await;

    let build = fx.create_storybook_build("sb0bad2").await;
    let build_id = build_id_of(&build);

    let res = fx.upload_bundle(build_id, b"i am not a zip".to_vec()).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "non-zip upload");

    // 弾かれた以上、ビルドは pending のままで finalize もできない。
    let build = fx.get_build(build_id).await;
    assert_eq!(build["status"].as_str(), Some("pending"));
    assert_eq!(build["storybook_uploaded"].as_bool(), Some(false));
    assert_eq!(
        fx.finalize(build_id).await.status(),
        StatusCode::BAD_REQUEST
    );
}

/// screenshots モードのビルドにバンドルを投げるのは 409。
#[tokio::test(flavor = "multi_thread")]
async fn storybook_upload_is_rejected_for_screenshot_mode_builds() {
    let fx = setup().await;

    let res = fx.create_build("screenshots", "sb0scr1").await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let build: Value = res.json().await.expect("build json");
    assert_eq!(
        build["mode"].as_str(),
        Some("screenshots"),
        "explicit screenshots mode"
    );
    let build_id = build_id_of(&build);

    assert_eq!(
        fx.upload_bundle(build_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
}

/// `mode` を省略したビルドは従来どおり screenshots モードになる（後方互換）。
#[tokio::test(flavor = "multi_thread")]
async fn omitting_mode_keeps_the_screenshots_behaviour() {
    let fx = setup().await;

    let res = fx
        .app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                fx.tenant_slug, fx.project_slug
            ),
            &fx.token,
            json!({ "branch": "main", "commit_sha": "sb0dflt" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let build: Value = res.json().await.expect("build json");
    assert_eq!(build["mode"].as_str(), Some("screenshots"));
    assert_eq!(build["storybook_uploaded"].as_bool(), Some(false));
}
