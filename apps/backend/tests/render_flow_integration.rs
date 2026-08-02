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

/// ストーリー 3 件 + docs 1 件の最小 index。
///
/// `demo-box--empty` は**何も描かない**ストーリー。DOM ヒューリスティックだけの
/// 旧実装ではここで 30 秒タイムアウトし、ビルドが丸ごと落ちていた
/// （実バンドルの `auth-passwordstrengthbar--empty` と同じ形）。
const INDEX_JSON: &str = r#"{
  "v": 5,
  "entries": {
    "demo-box--empty": {
      "type": "story",
      "id": "demo-box--empty",
      "title": "Demo/Box",
      "name": "Empty",
      "importPath": "./src/Box.stories.tsx"
    },
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

/// [`INDEX_JSON`] に新しいストーリー `demo-box--green` を 1 件足した index。
///
/// only_story_ids に**入れていない**新規ストーリーがちゃんと撮影される
/// （= 見逃されない）ことを確かめるために使う。
const INDEX_JSON_WITH_EXTRA: &str = r#"{
  "v": 5,
  "entries": {
    "demo-box--empty": {
      "type": "story",
      "id": "demo-box--empty",
      "title": "Demo/Box",
      "name": "Empty",
      "importPath": "./src/Box.stories.tsx"
    },
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
    "demo-box--green": {
      "type": "story",
      "id": "demo-box--green",
      "title": "Demo/Box",
      "name": "Green",
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
/// - 本物の Storybook と同じく `window.__STORYBOOK_ADDONS_CHANNEL__` を**後から**代入し、
///   描画後に `storyRendered` を撃つ。レンダラのシグナル経路を通す
/// - `demo-box--empty` は何も描かずにシグナルだけ出す（空ストーリーは正当な白紙）
fn iframe_html(red: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><style>html,body{{margin:0;padding:0;background:#fff}}</style></head>
<body>
<div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var listeners = {{}};
  var channel = {{
    on: function (event, cb) {{ (listeners[event] = listeners[event] || []).push(cb); }},
    emit: function (event, payload) {{
      (listeners[event] || []).forEach(function (cb) {{ cb(payload); }});
    }}
  }};
  var colors = {{ 'demo-box--red': '{red}', 'demo-box--blue': '#0000ff' }};
  setTimeout(function () {{
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    if (id !== 'demo-box--empty') {{
      var el = document.createElement('div');
      el.style.width = '100vw';
      el.style.height = '100vh';
      el.style.background = colors[id] || '#00ff00';
      document.getElementById('storybook-root').appendChild(el);
    }}
    channel.emit('storyRendered', id);
  }}, 0);
</script>
</body>
</html>"#
    )
}

/// バンドルに混ぜる「実バンドルらしさ」のためのダミー資産のサイズ。
///
/// axum の `DefaultBodyLimit` 既定値（2MB）を必ず超えるようにしてある。
/// 実際の storybook-static は JS チャンクだけで軽く数 MB あり、
/// 上限を上げ忘れると 400（`Error parsing multipart/form-data request`）になる。
/// この定数のおかげで通常の統合テストが常にその回帰を踏む。
const PADDING_BYTES: usize = 2_621_440; // 2.5MiB

/// 圧縮の効かないバイト列（xorshift による決定的な擬似乱数）。
///
/// Deflate で縮んでしまうと転送量が上限を超えず、回帰テストにならない。
fn incompressible_bytes(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// `storybook-static/` 相当を zip に固める（アーカイブ直下に index.json を置く形）。
///
/// `index.json` / `iframe.html` に加えて、非圧縮（stored）の 2.5MiB ダミー資産を
/// 1 つ入れる。これでアップロードは必ず 2MB を超え、`DefaultBodyLimit` の
/// 引き上げ漏れがテストで検出できる。
fn bundle_zip(red: &str) -> Vec<u8> {
    bundle_zip_with_index(INDEX_JSON, red)
}

/// [`bundle_zip`] と同じだが index.json を差し替えられる版。
fn bundle_zip_with_index(index_json: &str, red: &str) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in [
            ("index.json", index_json.to_string()),
            ("iframe.html", iframe_html(red)),
        ] {
            writer.start_file(name, options).expect("start zip entry");
            writer
                .write_all(contents.as_bytes())
                .expect("write zip entry");
        }

        // Stored（無圧縮）で入れる。zip 全体が 2.5MiB 超になる。
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("assets/chunk.bin", stored)
            .expect("start padding entry");
        writer
            .write_all(&incompressible_bytes(PADDING_BYTES))
            .expect("write padding entry");

        writer.finish().expect("finish zip");
    }
    let zip = buf.into_inner();
    assert!(
        zip.len() > PADDING_BYTES,
        "padded bundle must exceed the old 2MB body limit (got {} bytes)",
        zip.len()
    );
    zip
}

// ── セットアップ ────────────────────────────────────────────────────────

struct Fixture {
    app: TestApp,
    tenant_slug: String,
    project_slug: String,
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
    let project_id: Uuid = project["id"].as_str().expect("project id").parse().unwrap();
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
        project_id,
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

    /// `only_story_ids` を添えて finalize する（撮影対象を絞る）。
    ///
    /// 部分レンダリングは計画の起点 baseline の照合が必須になったため、
    /// `expected_baseline_commit_sha`（= 差分計画の起点にした baseline の
    /// 昇格元コミット）を常に添える。
    async fn finalize_with_only(
        &self,
        build_id: Uuid,
        only_story_ids: &[&str],
        expected_baseline_commit_sha: &str,
    ) -> reqwest::Response {
        self.app
            .post_json_with_bearer(
                &format!("/v1/ci/builds/{build_id}/finalize"),
                &self.token,
                json!({
                    "only_story_ids": only_story_ids,
                    "expected_baseline_commit_sha": expected_baseline_commit_sha,
                }),
            )
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
                assert_completed_at_is_stamped(&build);
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

    /// サーバーのジョブが追記した build_logs 行を DB から直接読む（id 昇順）。
    async fn build_logs(&self, build_id: Uuid) -> Vec<entity::build_logs::Model> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        entity::build_logs::Entity::find()
            .filter(entity::build_logs::Column::BuildId.eq(build_id))
            .order_by_asc(entity::build_logs::Column::Id)
            .all(&self.app.state.db)
            .await
            .expect("load build logs")
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

    /// 最新 baseline の ID と、その `Demo/Box/Red` エントリのストレージキー。
    async fn latest_baseline_red_entry(&self) -> (Uuid, String) {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let baseline = entity::baselines::Entity::find()
            .filter(entity::baselines::Column::ProjectId.eq(self.project_id))
            .order_by_desc(entity::baselines::Column::CreatedAt)
            .order_by_desc(entity::baselines::Column::Id)
            .one(&self.app.state.db)
            .await
            .expect("query baseline")
            .expect("baseline exists");

        let entry = entity::baseline_entries::Entity::find()
            .filter(entity::baseline_entries::Column::BaselineId.eq(baseline.id))
            .filter(entity::baseline_entries::Column::Name.eq("Demo/Box/Red"))
            .one(&self.app.state.db)
            .await
            .expect("query red entry")
            .expect("red entry exists");

        (baseline.id, entry.storage_key)
    }
}

/// パイプラインが完走した状態なら `completed_at` が必ず入っていること。
///
/// `changes_detected` は `is_terminal()` が false なので、旧実装ではここだけ
/// NULL のまま残っていた（実機のドッグフードで発覚）。
fn assert_completed_at_is_stamped(build: &Value) {
    let status = build["status"].as_str().unwrap_or_default();
    if !matches!(status, "passed" | "changes_detected" | "failed") {
        return;
    }
    assert!(
        build["completed_at"].as_str().is_some(),
        "build in status {status:?} must carry completed_at, got {:?}",
        build["completed_at"]
    );
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
        Some(3),
        "docs entry skipped"
    );
    assert_eq!(build1["added_count"].as_i64(), Some(3));

    // サーバーが撮ったスクリーンショットは `{title}/{name}` で並ぶ。
    // 何も描かない Empty も 1 枚（白紙）として撮れていること。
    let shots = fx.screenshots(build1_id).await;
    let names: Vec<&str> = shots.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Demo/Box/Blue", "Demo/Box/Empty", "Demo/Box/Red"]
    );

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
    let approved: Value = res.json().await.expect("approve json");

    // 承認は「自動処理が終わった時刻」を動かさない。承認の時刻は approved_at。
    assert_eq!(
        approved["completed_at"], build1["completed_at"],
        "approving must not overwrite completed_at (it is the pipeline finish time)"
    );
    assert!(
        approved["approved_at"].as_str().is_some(),
        "approving must stamp approved_at"
    );

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
    assert_eq!(build2["unchanged_count"].as_i64(), Some(3));

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
    assert_eq!(build3["unchanged_count"].as_i64(), Some(2));

    let cmps = fx.comparisons(build3_id).await;
    let changed: Vec<&str> = cmps
        .iter()
        .filter(|c| c["status"] == "changed")
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert_eq!(changed, vec!["Demo/Box/Red"], "only the red story changed");
}

/// `only_story_ids` を指定すると、対象外のストーリーは baseline を流用し、
/// 新規ストーリーだけは（指定外でも）撮影される（TurboSnap 相当）。
#[tokio::test(flavor = "multi_thread")]
async fn only_story_ids_reuses_baseline_and_still_renders_new_stories() {
    if !chromium_or_skip("only_story_ids_reuses_baseline_and_still_renders_new_stories") {
        return;
    }
    let fx = setup().await;

    // ── ビルド A: 全撮影 → 承認して baseline 化 ───────────────────────────
    let build_a = fx.create_storybook_build("only001").await;
    let build_a_id = build_id_of(&build_a);
    assert_eq!(
        fx.upload_bundle(build_a_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_a_id).await.status(), StatusCode::OK);
    let build_a = fx.wait_for_terminal(build_a_id).await;
    assert_eq!(build_a["status"].as_str(), Some("changes_detected"));
    assert_eq!(build_a["added_count"].as_i64(), Some(3));

    let res = fx
        .app
        .post_json(
            &format!("/v1/builds/{build_a_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "force approve build A");

    // ── ビルド B: 同じ bundle + only_story_ids に Red だけ指定 ────────────
    // Red は撮影、Blue / Empty は baseline を流用する。
    let build_b = fx.create_storybook_build("only002").await;
    let build_b_id = build_id_of(&build_b);
    assert_eq!(
        fx.upload_bundle(build_b_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    // 計画の起点 = build A から昇格した baseline（コミット only001）。
    let res = fx
        .finalize_with_only(build_b_id, &["demo-box--red"], "only001")
        .await;
    assert_eq!(res.status(), StatusCode::OK, "finalize with only_story_ids");
    let finalized: Value = res.json().await.expect("finalize json");
    assert_eq!(
        finalized["status"].as_str(),
        Some("rendering"),
        "storybook finalize goes to rendering"
    );

    let build_b = fx.wait_for_terminal(build_b_id).await;
    // 同じ bundle なので撮影ぶんも流用ぶんも baseline と一致 → 全部 unchanged。
    assert_eq!(
        build_b["status"].as_str(),
        Some("passed"),
        "reused + re-rendered identical bytes are all unchanged (error: {:?})",
        build_b["error_message"]
    );
    assert_eq!(build_b["total_count"].as_i64(), Some(3));
    assert_eq!(build_b["unchanged_count"].as_i64(), Some(3));

    // 指定外の Blue / Empty は流用（reused: true）、Red は撮影（reused: false）。
    let shots = fx.screenshots(build_b_id).await;
    let names: Vec<&str> = shots.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Demo/Box/Blue", "Demo/Box/Empty", "Demo/Box/Red"],
        "all three stories have a screenshot for this build"
    );
    for shot in &shots {
        let reused = shot
            .metadata
            .as_ref()
            .and_then(|m| m.get("reused"))
            .and_then(|v| v.as_bool());
        let expected = shot.name != "Demo/Box/Red";
        assert_eq!(
            reused,
            Some(expected),
            "screenshot {} reused flag ({expected} expected)",
            shot.name
        );
    }

    // 比較は全ストーリーぶん揃い、流用ぶんも unchanged になっている。
    let cmps = fx.comparisons(build_b_id).await;
    assert_eq!(cmps.len(), 3, "comparisons cover every story");
    assert!(
        cmps.iter().all(|c| c["status"] == "unchanged"),
        "every comparison is unchanged, got {cmps:?}"
    );

    // 部分撮影ビルドを承認する。Reuse が参照だけで screenshot 実体を作らなければ、
    // baseline manifest の Blue / Empty が欠落扱いになり、この承認は 409 で落ちる。
    let res = fx
        .app
        .post_json(&format!("/v1/builds/{build_b_id}/approve"), json!({}))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "reused screenshots are physical build artifacts accepted by the manifest guard"
    );

    // ── ビルド C: 新規ストーリー Green を足す。only_story_ids に入れない ───
    // baseline に Green が無いので、指定外でも撮影されて added になる。
    let build_c = fx.create_storybook_build("only003").await;
    let build_c_id = build_id_of(&build_c);
    assert_eq!(
        fx.upload_bundle(
            build_c_id,
            bundle_zip_with_index(INDEX_JSON_WITH_EXTRA, "#ff0000")
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    // Red だけ指定。Green は指定に入れない（新規なので撮影されるはず）。
    // 直前に build B（コミット only002）が承認されて baseline が進んでいる。
    assert_eq!(
        fx.finalize_with_only(build_c_id, &["demo-box--red"], "only002")
            .await
            .status(),
        StatusCode::OK
    );

    let build_c = fx.wait_for_terminal(build_c_id).await;
    assert_eq!(
        build_c["status"].as_str(),
        Some("changes_detected"),
        "new story is added, so build has differences (error: {:?})",
        build_c["error_message"]
    );
    assert_eq!(build_c["total_count"].as_i64(), Some(4));
    assert_eq!(build_c["added_count"].as_i64(), Some(1));
    assert_eq!(build_c["unchanged_count"].as_i64(), Some(3));

    let shots = fx.screenshots(build_c_id).await;
    let green = shots
        .iter()
        .find(|s| s.name == "Demo/Box/Green")
        .expect("new Green story was rendered");
    // 新規ストーリーは baseline に無いので流用ではなく撮影される。
    assert_eq!(
        green
            .metadata
            .as_ref()
            .and_then(|m| m.get("reused"))
            .and_then(|v| v.as_bool()),
        Some(false),
        "new story must be rendered, not reused"
    );

    let cmps = fx.comparisons(build_c_id).await;
    let added: Vec<&str> = cmps
        .iter()
        .filter(|c| c["status"] == "added")
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert_eq!(added, vec!["Demo/Box/Green"], "only the new story is added");
}

/// storybook の流用（`only_story_ids`）は、finalize が照合のうえ固定した baseline を使う。
///
/// 作成〜finalize の間に別ビルドが承認されて最新 baseline が入れ替わった場合、
/// 古い起点のままの finalize は 409 で拒否される（流用画像と比較対象が計画と
/// 別物になる前に断つ。「baseline が動いた」は plan 添付と同じ 409）。現在の baseline へ再計画すれば、その baseline が固定され、
/// 流用画像・比較対象・`baseline_id` のすべてが一致する。
///
/// positive control: 起点の照合をせず finalize 時の最新へ黙って倒す実装では、
/// 古い起点（pin0001）の finalize が 200 で通ってしまい、最初のアサートで落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn stale_reuse_basis_is_rejected_and_repinning_follows_the_current_baseline() {
    if !chromium_or_skip("stale_reuse_basis_is_rejected_and_repinning_follows_the_current_baseline")
    {
        return;
    }
    let fx = setup().await;

    // ── ビルド A: 明るい赤で全撮影 → 承認して baseline B1 を確立 ───────────
    let build_a = fx.create_storybook_build("pin0001").await;
    let build_a_id = build_id_of(&build_a);
    assert_eq!(
        fx.upload_bundle(build_a_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_a_id).await.status(), StatusCode::OK);
    fx.wait_for_terminal(build_a_id).await;
    let res = fx
        .app
        .post_json(
            &format!("/v1/builds/{build_a_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve build A");
    let (old_baseline_id, red_v1_key) = fx.latest_baseline_red_entry().await;

    // ── ビルド B: この時点で作成（差分計画の起点は B1 のつもり）─────────────
    let build_b = fx.create_storybook_build("pin0002").await;
    let build_b_id = build_id_of(&build_b);
    assert_eq!(
        fx.upload_bundle(build_b_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );

    // ── ビルド C: 暗い赤で全撮影 → 承認して最新 baseline を B2 へ動かす ────
    let build_c = fx.create_storybook_build("pin0003").await;
    let build_c_id = build_id_of(&build_c);
    assert_eq!(
        fx.upload_bundle(build_c_id, bundle_zip("#880000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_c_id).await.status(), StatusCode::OK);
    fx.wait_for_terminal(build_c_id).await;
    let res = fx
        .app
        .post_json(
            &format!("/v1/builds/{build_c_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve build C");
    let (moved_baseline_id, red_v2_key) = fx.latest_baseline_red_entry().await;
    assert_ne!(old_baseline_id, moved_baseline_id, "baseline moved");

    // ── 古い起点（B1 = pin0001）のままの部分 finalize は 409 ───────────────
    // 「baseline が動いた（再計画で解消）」は plan 添付と同じ 409 に揃えてある。
    let res = fx
        .finalize_with_only(build_b_id, &["demo-box--blue"], "pin0001")
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "a partial render planned against a baseline that has since moved must be rejected"
    );

    // ── 現在の baseline（B2 = pin0003）へ再計画すれば通り、B2 が固定される ──
    assert_eq!(
        fx.finalize_with_only(build_b_id, &["demo-box--blue"], "pin0003")
            .await
            .status(),
        StatusCode::OK
    );
    let build_b = fx.wait_for_terminal(build_b_id).await;
    assert_eq!(
        build_b["baseline_id"].as_str().map(|s| s.parse().unwrap()),
        Some(moved_baseline_id),
        "the build must record the baseline verified and pinned at finalize"
    );

    // 流用された Red のバイト列は固定した現行 baseline（暗い赤）と一致し、
    // 旧 baseline（明るい赤）とは一致しない——流用元と照合済みの起点が同一。
    let shots = fx.screenshots(build_b_id).await;
    let red_shot = shots
        .iter()
        .find(|s| s.name == "Demo/Box/Red")
        .expect("reused red screenshot");
    let reused = service::screenshots::read_all(&fx.app.state.storage, &red_shot.storage_key)
        .await
        .expect("read reused bytes");
    let v1 = service::screenshots::read_all(&fx.app.state.storage, &red_v1_key)
        .await
        .expect("read old baseline bytes");
    let v2 = service::screenshots::read_all(&fx.app.state.storage, &red_v2_key)
        .await
        .expect("read current baseline bytes");
    assert_eq!(
        reused, v2,
        "reuse must copy from the baseline pinned at finalize (dark red)"
    );
    assert_ne!(
        reused, v1,
        "reuse must not silently keep a stale planning basis"
    );
}

/// リトライで届いた 2 度目の部分 finalize は、固定済みの baseline を上書きできない。
///
/// 「pending 再確認 → SHA 照合 → pin → 遷移」が build 行ロックの 1 トランザクション
/// になっていないと、finalize 済みビルドへ届いた 2 度目の finalize（起点 = その後
/// 前進した現行 baseline）が SHA 照合を通過して `baseline_id` だけを先に上書きし、
/// 本体は 409 で弾かれてもレンダリング・比較ジョブは以後 新しい baseline を読む
/// ——計画の根拠と比較相手がずれる。
///
/// positive control: pin を遷移チェックの前に単独コミットしていた旧実装では、
/// 2 度目の finalize 後の `baseline_id` が B2 に化け、最後のアサートで落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn a_second_finalize_cannot_overwrite_the_pinned_baseline() {
    if !chromium_or_skip("a_second_finalize_cannot_overwrite_the_pinned_baseline") {
        return;
    }
    let fx = setup().await;

    // ── ビルド A: 全撮影 → 承認して baseline B1 を確立 ─────────────────────
    let build_a = fx.create_storybook_build("repin001").await;
    let build_a_id = build_id_of(&build_a);
    assert_eq!(
        fx.upload_bundle(build_a_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_a_id).await.status(), StatusCode::OK);
    fx.wait_for_terminal(build_a_id).await;
    let res = fx
        .app
        .post_json(
            &format!("/v1/builds/{build_a_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve build A");
    let (pinned_baseline_id, _) = fx.latest_baseline_red_entry().await;

    // ── ビルド B: B1 を起点に部分 finalize（1 度目）→ B1 が固定される ──────
    let build_b = fx.create_storybook_build("repin002").await;
    let build_b_id = build_id_of(&build_b);
    assert_eq!(
        fx.upload_bundle(build_b_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        fx.finalize_with_only(build_b_id, &["demo-box--blue"], "repin001")
            .await
            .status(),
        StatusCode::OK,
        "first finalize pins B1"
    );
    fx.wait_for_terminal(build_b_id).await;

    // ── ビルド C: 暗い赤で全撮影 → 承認して最新 baseline を B2 へ動かす ────
    let build_c = fx.create_storybook_build("repin003").await;
    let build_c_id = build_id_of(&build_c);
    assert_eq!(
        fx.upload_bundle(build_c_id, bundle_zip("#880000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_c_id).await.status(), StatusCode::OK);
    fx.wait_for_terminal(build_c_id).await;
    let res = fx
        .app
        .post_json(
            &format!("/v1/builds/{build_c_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve build C");
    let (moved_baseline_id, _) = fx.latest_baseline_red_entry().await;
    assert_ne!(pinned_baseline_id, moved_baseline_id, "baseline moved to B2");

    // ── 2 度目の finalize（起点 = 現行 B2）は 409 で、pin を上書きしない ────
    // 現行 baseline の SHA を正しく持ってきても、finalize 済みビルドの pin は動かせない。
    let res = fx
        .finalize_with_only(build_b_id, &["demo-box--blue"], "repin003")
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "a second finalize on an already-finalized build must be rejected"
    );

    use sea_orm::EntityTrait;
    let row = entity::builds::Entity::find_by_id(build_b_id)
        .one(&fx.app.state.db)
        .await
        .expect("query build B")
        .expect("build B exists");
    assert_eq!(
        row.baseline_id,
        Some(pinned_baseline_id),
        "the rejected second finalize must not overwrite the pinned baseline \
         (render/compare jobs read baseline_id; silently repinning to B2 would \
         desync the reuse basis from the comparison target)"
    );
}

/// `only_story_ids` に `expected_baseline_commit_sha` を添えない finalize は 400。
///
/// 後方互換を破ってでも照合を必須にした当のガードの否定側。バンドルの有無より
/// 前に入口で効く（照合できない部分レンダリングを開始させない）。
#[tokio::test(flavor = "multi_thread")]
async fn only_story_ids_without_expected_baseline_sha_is_rejected() {
    let fx = setup().await;
    let build = fx.create_storybook_build("neg00001").await;
    let build_id = build_id_of(&build);

    let res = fx
        .app
        .post_json_with_bearer(
            &format!("/v1/ci/builds/{build_id}/finalize"),
            &fx.token,
            json!({ "only_story_ids": ["demo-box--red"] }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a partial render without its planning basis cannot be verified or pinned"
    );
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("requires expected_baseline_commit_sha"),
        "the error must demand the planning basis: {body}"
    );
}

/// `storybook` モードに `captured_names` を渡すと 400（サーバーが撮るので
/// 「CI が撮った名前」の宣言は成立しない）。
#[tokio::test(flavor = "multi_thread")]
async fn captured_names_is_rejected_for_storybook_mode() {
    let fx = setup().await;
    let build = fx.create_storybook_build("neg00002").await;
    let build_id = build_id_of(&build);

    let res = fx
        .app
        .post_json_with_bearer(
            &format!("/v1/ci/builds/{build_id}/finalize"),
            &fx.token,
            json!({ "captured_names": ["Demo/Box/Red"] }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "captured_names is a screenshots-mode declaration"
    );
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("not supported for storybook-mode"),
        "the error must point at the mode mismatch: {body}"
    );
}

/// `screenshots` モードに `only_story_ids` を渡すと 400（サーバー撮影しない）。
#[tokio::test(flavor = "multi_thread")]
async fn only_story_ids_is_rejected_for_screenshot_mode() {
    let fx = setup().await;

    let res = fx.create_build("screenshots", "onlyscr").await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let build: Value = res.json().await.expect("build json");
    let build_id = build_id_of(&build);

    let res = fx
        .finalize_with_only(build_id, &["button--primary"], "whatever")
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "only_story_ids is meaningless for screenshots mode"
    );
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

/// 2MB を超えるバンドルが受け付けられる（axum の既定ボディ上限の回帰テスト）。
///
/// `DefaultBodyLimit` を上げていないと `Multipart` が 2MB で読み込みを打ち切り、
/// ハンドラは 400（`invalid file field: Error parsing multipart/form-data request`）
/// を返す。実際の storybook-static は数 MB〜数十 MB あるので、これが本番の障害だった。
/// Chromium を必要としないので、どの環境でも必ず走る。
#[tokio::test(flavor = "multi_thread")]
async fn bundles_larger_than_the_default_body_limit_are_accepted() {
    let fx = setup().await;

    let build = fx.create_storybook_build("sb0big1").await;
    let build_id = build_id_of(&build);

    let zip = bundle_zip("#ff0000");
    assert!(
        zip.len() > 2 * 1024 * 1024,
        "fixture must exceed axum's 2MB default body limit, got {} bytes",
        zip.len()
    );

    let res = fx.upload_bundle(build_id, zip).await;
    assert_eq!(
        res.status(),
        StatusCode::CREATED,
        "multi-megabyte bundle upload"
    );

    let build = fx.get_build(build_id).await;
    assert_eq!(build["storybook_uploaded"].as_bool(), Some(true));
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

// ── ビルド進捗ログ ────────────────────────────────────────────────────────

/// 件数の数え上げ（ログメッセージの接頭辞ごと）。
fn count_prefix(logs: &[entity::build_logs::Model], prefix: &str) -> usize {
    logs.iter()
        .filter(|l| l.message.starts_with(prefix))
        .count()
}

/// render / compare のジョブが進捗ログを行単位で残し、rendered / reused の行が
/// 期待どおりの数だけ出ること。CI/UI の増分取得エンドポイントも通す。
#[tokio::test(flavor = "multi_thread")]
async fn build_logs_capture_render_and_compare_progress() {
    if !chromium_or_skip("build_logs_capture_render_and_compare_progress") {
        return;
    }
    let fx = setup().await;

    // ── ビルド A: 全撮影 → 承認して baseline 化 ───────────────────────────
    let build_a = fx.create_storybook_build("log0001").await;
    let build_a_id = build_id_of(&build_a);
    assert_eq!(
        fx.upload_bundle(build_a_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_a_id).await.status(), StatusCode::OK);
    fx.wait_for_terminal(build_a_id).await;

    let logs_a = fx.build_logs(build_a_id).await;
    // 開始 1 + rendered 3 + render complete 1、加えて compare 側の start/summary。
    assert_eq!(
        count_prefix(&logs_a, "render started"),
        1,
        "one render-start line, got {logs_a:?}"
    );
    assert_eq!(
        count_prefix(&logs_a, "rendered "),
        3,
        "three stories rendered on the first build"
    );
    assert_eq!(count_prefix(&logs_a, "reused "), 0, "nothing reused yet");
    assert_eq!(count_prefix(&logs_a, "render complete"), 1);
    assert_eq!(count_prefix(&logs_a, "compare started"), 1);
    assert_eq!(count_prefix(&logs_a, "compare complete"), 1);
    // ログは id 昇順（= 追記順）で並ぶ。
    assert!(
        logs_a.windows(2).all(|w| w[0].id < w[1].id),
        "logs are strictly ascending by id"
    );

    let res = fx
        .app
        .post_json(
            &format!("/v1/builds/{build_a_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "force approve build A");

    // ── ビルド B: only_story_ids に Red だけ → 1 撮影 + 2 流用 ─────────────
    let build_b = fx.create_storybook_build("log0002").await;
    let build_b_id = build_id_of(&build_b);
    assert_eq!(
        fx.upload_bundle(build_b_id, bundle_zip("#ff0000"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        fx.finalize_with_only(build_b_id, &["demo-box--red"], "log0001")
            .await
            .status(),
        StatusCode::OK
    );
    fx.wait_for_terminal(build_b_id).await;

    let logs_b = fx.build_logs(build_b_id).await;
    assert_eq!(
        count_prefix(&logs_b, "rendered "),
        1,
        "only Red is rendered, got {logs_b:?}"
    );
    assert_eq!(
        count_prefix(&logs_b, "reused "),
        2,
        "Blue and Empty are reused from baseline"
    );

    // ── 増分取得エンドポイント（セッション認証）────────────────────────────
    let res = fx.app.get(&format!("/v1/builds/{build_a_id}/logs")).await;
    assert_eq!(res.status(), StatusCode::OK, "list build logs");
    let body: Value = res.json().await.expect("logs json");
    let entries = body["entries"].as_array().expect("entries array");
    assert_eq!(
        entries.len(),
        logs_a.len(),
        "endpoint returns every recorded line"
    );
    let last_id = body["last_id"].as_i64().expect("last_id");
    assert_eq!(
        last_id,
        entries.last().and_then(|e| e["id"].as_i64()).unwrap(),
        "last_id points at the final entry"
    );
    assert_eq!(entries[0]["level"].as_str(), Some("info"));

    // after=last_id なら新着なし、カーソルは据え置き。
    let res = fx
        .app
        .get(&format!("/v1/builds/{build_a_id}/logs?after={last_id}"))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.expect("logs json");
    assert_eq!(
        body["entries"].as_array().map(|a| a.len()),
        Some(0),
        "no new lines past the cursor"
    );
    assert_eq!(body["last_id"].as_i64(), Some(last_id), "cursor held");

    // ── CI トークン経由の増分取得も同形 ────────────────────────────────────
    let res = fx
        .app
        .get_with_bearer(&format!("/v1/ci/builds/{build_a_id}/logs"), &fx.token)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "ci logs endpoint");
    let body: Value = res.json().await.expect("ci logs json");
    assert_eq!(
        body["entries"].as_array().map(|a| a.len()),
        Some(logs_a.len())
    );
}

/// 他テナントの CI トークンでは他人のビルドのログを引けない（403）。
///
/// Chromium 不要。ビルドは finalize せず pending のままでよい（認可の検査だけ）。
#[tokio::test(flavor = "multi_thread")]
async fn ci_build_logs_endpoint_is_tenant_scoped() {
    // ビルドの所有者。
    let owner = setup().await;
    let build = owner.create_storybook_build("logacl1").await;
    let build_id = build_id_of(&build);

    // 別テナント・別ユーザー・別トークン（DB はプロセス内で共有される）。
    let outsider = setup().await;

    // 部外者の CI トークンでは、存在するビルドでも 403（存在の有無を漏らさない）。
    let res = outsider
        .app
        .get_with_bearer(&format!("/v1/ci/builds/{build_id}/logs"), &outsider.token)
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "outsider must not read another tenant's build logs (ci)"
    );

    // UI 側（セッション認証）の口も同じくテナント境界で弾く。
    let res = outsider
        .app
        .get(&format!("/v1/builds/{build_id}/logs"))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "outsider must not read another tenant's build logs (ui)"
    );

    // 所有者本人は自分のビルドのログ口に 200 でアクセスできる。
    let res = owner
        .app
        .get_with_bearer(&format!("/v1/ci/builds/{build_id}/logs"), &owner.token)
        .await;
    assert_eq!(res.status(), StatusCode::OK, "owner can read its own logs");
}
