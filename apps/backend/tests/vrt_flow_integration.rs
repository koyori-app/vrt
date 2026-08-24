//! VRT コアフローの E2E 統合テスト（Phase 5 の受け入れゲート）。
//!
//! 本物の Postgres / Valkey（testcontainers）+ ローカルストレージ + apalis ワーカーで、
//! CI からのビルド作成 → アップロード → finalize → 比較ジョブ → レビュー → baseline 昇格
//! までを通す。
//!
//! シナリオ:
//!
//! 1. ビルド #1: 初回なので全部 `added` → `changes_detected` → force 承認で baseline (2 件)
//! 2. ビルド #2: 1 枚同一 / 1 枚変更 / 1 枚新規 → `unchanged:1, changed:1, added:1`
//!    → 個別レビュー → 承認で baseline (3 件)
//! 3. ビルド #3: #2 と同じ 3 枚 → `passed`（`unchanged:3`、レビュー不要）

mod common;

use std::time::Duration;

use common::TestApp;
use entity::scopes::Scope;
use image::{Rgba, RgbaImage};
use reqwest::StatusCode;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};
use uuid::Uuid;

/// ワーカーの処理待ちのタイムアウト。
const POLL_TIMEOUT: Duration = Duration::from_secs(60);

// ── 画像生成ヘルパー ────────────────────────────────────────────────────

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    encode(&RgbaImage::from_pixel(width, height, Rgba(color)))
}

/// 単色の中に矩形を描いた PNG（「変更あり」を作るのに使う）。
fn png_with_rect(
    width: u32,
    height: u32,
    background: [u8; 4],
    rect: (u32, u32, u32, u32),
    color: [u8; 4],
) -> Vec<u8> {
    let mut image = RgbaImage::from_pixel(width, height, Rgba(background));
    let (x0, y0, w, h) = rect;
    for y in y0..(y0 + h).min(height) {
        for x in x0..(x0 + w).min(width) {
            image.put_pixel(x, y, Rgba(color));
        }
    }
    encode(&image)
}

/// 圧縮の効かないノイズ PNG。エンコード後でも 2.5MiB を超える。
///
/// axum の `DefaultBodyLimit`（既定 2MB）を実際に踏み越えるためのフィクスチャなので、
/// 単色 PNG のように縮んでしまっては意味がない。決定的な xorshift でノイズを作る。
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut image = RgbaImage::new(width, height);
    for pixel in image.pixels_mut() {
        let r = next().to_le_bytes();
        *pixel = Rgba([r[0], r[1], r[2], 255]);
    }
    encode(&image)
}

/// パイプラインが完走した状態なら `completed_at` が必ず入っていること。
///
/// `completed_at` は「自動処理が終わった時刻」。`changes_detected` は
/// `is_terminal()` が false（レビューでまだ動く）なので、旧実装ではここだけ
/// NULL のまま残っていた。`created_at → completed_at` を所要時間として読むには、
/// 3 つの完走状態すべてで埋まっている必要がある。
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

fn encode(image: &RgbaImage) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}

/// IEND の直前に tEXt チャンクを挿入してバイト列を変える。
/// ピクセルは同一のままハッシュだけが変わる。
fn inject_text_chunk(mut png: Vec<u8>) -> Vec<u8> {
    let iend_pos = png
        .windows(4)
        .rposition(|w| w == b"IEND")
        .expect("IEND chunk")
        - 4;
    let key_value = b"Comment\0injected";
    let data_len = (key_value.len() as u32).to_be_bytes();
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&data_len);
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(key_value);
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    png.splice(iend_pos..iend_pos, chunk);
    png
}

/// IHDR は保ったまま IDAT を壊す。寸法取得は通るが full decode は失敗する。
fn corrupt_idat(mut png: Vec<u8>) -> Vec<u8> {
    let mut offset = 8;
    while offset + 12 <= png.len() {
        let len = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
        let kind = &png[offset + 4..offset + 8];
        if kind == b"IDAT" && len > 0 {
            png[offset + 8 + len / 2] ^= 0xff;
            return png;
        }
        offset += 12 + len;
    }
    panic!("encoded PNG has no IDAT chunk");
}

// ── フローのヘルパー ────────────────────────────────────────────────────

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
    let tenant_slug = format!("vrt-{suffix}");
    let project_slug = format!("web-{suffix}");

    let res = app
        .post_json(
            "/v1/tenants",
            json!({ "name": "VRT Co", "slug": tenant_slug }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "create tenant");
    let tenant: Value = res.json().await.expect("tenant json");
    let tenant_id = tenant["id"].as_str().expect("tenant id").to_string();

    let res = app
        .post_json(
            &format!("/v1/tenants/{tenant_id}/projects"),
            json!({ "name": "Web", "slug": project_slug }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "create project");
    let project: Value = res.json().await.expect("project json");
    let project_id: Uuid = project["id"].as_str().expect("project id").parse().unwrap();

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
    async fn create_build(&self, branch: &str, sha: &str) -> Value {
        let res = self
            .app
            .post_json_with_bearer(
                &format!(
                    "/v1/ci/projects/{}/{}/builds",
                    self.tenant_slug, self.project_slug
                ),
                &self.token,
                json!({ "branch": branch, "commit_sha": sha }),
            )
            .await;
        assert_eq!(res.status(), StatusCode::CREATED, "create build");
        res.json().await.expect("build json")
    }

    async fn upload(&self, build_id: Uuid, name: &str, png: Vec<u8>) -> StatusCode {
        self.app
            .upload_screenshot(build_id, &self.token, name, png)
            .await
            .status()
    }

    async fn finalize(&self, build_id: Uuid) -> StatusCode {
        self.app
            .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &self.token)
            .await
            .status()
    }

    /// CI 用のポーリングエンドポイントで終端状態になるまで待つ。
    async fn wait_for_terminal(&self, build_id: Uuid) -> Value {
        let deadline = std::time::Instant::now() + POLL_TIMEOUT;
        loop {
            let res = self
                .app
                .get_with_bearer(&format!("/v1/ci/builds/{build_id}"), &self.token)
                .await;
            assert_eq!(res.status(), StatusCode::OK, "poll build status");
            let build: Value = res.json().await.expect("build json");
            let status = build["status"].as_str().unwrap_or_default().to_string();

            if !matches!(status.as_str(), "pending" | "queued" | "processing") {
                assert_completed_at_is_stamped(&build);
                return build;
            }
            if std::time::Instant::now() >= deadline {
                dump_apalis_state(&self.app.state.db).await;
                panic!("build {build_id} stuck in {status} after {POLL_TIMEOUT:?}");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
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

    async fn build_logs(&self, build_id: Uuid) -> Vec<Value> {
        let res = self
            .app
            .get_with_bearer(&format!("/v1/ci/builds/{build_id}/logs"), &self.token)
            .await;
        assert_eq!(res.status(), StatusCode::OK, "list build logs");
        let body: Value = res.json().await.expect("build logs json");
        body["entries"].as_array().expect("log entries").clone()
    }

    async fn approve(&self, build_id: Uuid, force: bool) -> reqwest::Response {
        self.app
            .post_json(
                &format!("/v1/builds/{build_id}/approve"),
                json!({ "force": force }),
            )
            .await
    }
}

/// タイムアウト時に apalis のジョブ行を吐き出す（診断用）。
async fn dump_apalis_state(db: &sea_orm::DatabaseConnection) {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    for sql in [
        "SELECT coalesce(string_agg(format('%s q=%s st=%s att=%s lock=%s', id, job_type, status, attempts, coalesce(lock_by,'-')), E'\n'), '(none)') FROM apalis.jobs",
        "SELECT coalesce(string_agg(format('worker id=%s layers=%s', id, layers), E'\n'), '(none)') FROM apalis.workers",
        "SELECT format('pg_stat_activity=%s max_connections=%s', (SELECT count(*) FROM pg_stat_activity), current_setting('max_connections'))",
    ] {
        let row = db
            .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await;
        match row {
            Ok(Some(r)) => eprintln!(
                "### {}",
                r.try_get_by_index::<String>(0)
                    .unwrap_or_else(|e| format!("<{e}>"))
            ),
            Ok(None) => eprintln!("### (no row)"),
            Err(e) => eprintln!("### ERR {e}"),
        }
    }
}

fn build_id_of(build: &Value) -> Uuid {
    build["id"].as_str().expect("build id").parse().unwrap()
}

fn counts(build: &Value) -> (i64, i64, i64, i64, i64) {
    (
        build["total_count"].as_i64().unwrap_or(-1),
        build["changed_count"].as_i64().unwrap_or(-1),
        build["added_count"].as_i64().unwrap_or(-1),
        build["removed_count"].as_i64().unwrap_or(-1),
        build["unchanged_count"].as_i64().unwrap_or(-1),
    )
}

fn find_comparison<'a>(list: &'a [Value], name: &str) -> &'a Value {
    list.iter()
        .find(|c| c["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("comparison {name} not found"))
}

// ── 本体 ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn vrt_full_flow_from_first_build_to_stable_baseline() {
    let fx = setup().await;

    // ── ビルド #1: 初回。baseline が無いので全部 added ───────────────────
    let build1 = fx.create_build("main", "aaaa1111").await;
    assert_eq!(build1["number"].as_i64(), Some(1), "first build is #1");
    assert_eq!(build1["status"].as_str(), Some("pending"));
    let build1_id = build_id_of(&build1);

    let home_v1 = png_with_rect(
        40,
        30,
        [255, 255, 255, 255],
        (2, 2, 10, 10),
        [0, 0, 255, 255],
    );
    let about_v1 = png(40, 30, [200, 200, 200, 255]);

    assert_eq!(
        fx.upload(build1_id, "home", home_v1.clone()).await,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.upload(build1_id, "about", about_v1.clone()).await,
        StatusCode::CREATED
    );

    assert_eq!(fx.finalize(build1_id).await, StatusCode::OK);

    let build1 = fx.wait_for_terminal(build1_id).await;
    assert_eq!(
        build1["status"].as_str(),
        Some("changes_detected"),
        "first build reports every screenshot as added"
    );
    assert_eq!(counts(&build1), (2, 0, 2, 0, 0), "(total,ch,add,rm,unch)");

    let cmps = fx.comparisons(build1_id).await;
    assert_eq!(cmps.len(), 2);
    assert!(cmps.iter().all(|c| c["status"] == "added"));
    assert!(cmps.iter().all(|c| c["review_status"] == "pending"));

    // 未レビューのまま force 無し承認 → 409
    let res = fx.approve(build1_id, false).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "approving with pending reviews requires force"
    );

    // force 承認 → baseline に 2 件昇格
    let res = fx.approve(build1_id, true).await;
    assert_eq!(res.status(), StatusCode::OK, "force approve");
    let approved: Value = res.json().await.expect("approved json");
    assert_eq!(approved["status"].as_str(), Some("approved"));
    assert!(approved["approved_at"].is_string());

    assert_eq!(
        baseline_entry_count(&fx).await,
        2,
        "baseline promoted with both screenshots"
    );

    // ── ビルド #2: 1 枚同一 / 1 枚変更 / 1 枚新規 ────────────────────────
    let build2 = fx.create_build("main", "bbbb2222").await;
    assert_eq!(
        build2["number"].as_i64(),
        Some(2),
        "build numbers increment"
    );
    let build2_id = build_id_of(&build2);

    // home は矩形の色を変える（明確な差分）
    let home_v2 = png_with_rect(
        40,
        30,
        [255, 255, 255, 255],
        (2, 2, 10, 10),
        [255, 0, 0, 255],
    );

    assert_eq!(
        fx.upload(build2_id, "about", about_v1.clone()).await,
        StatusCode::CREATED,
        "identical screenshot"
    );
    assert_eq!(
        fx.upload(build2_id, "home", home_v2).await,
        StatusCode::CREATED,
        "modified screenshot"
    );
    assert_eq!(
        fx.upload(build2_id, "pricing", png(20, 20, [10, 200, 10, 255]))
            .await,
        StatusCode::CREATED,
        "new screenshot"
    );

    assert_eq!(fx.finalize(build2_id).await, StatusCode::OK);

    // finalize 後のアップロードは 409
    assert_eq!(
        fx.upload(build2_id, "late", png(5, 5, [0, 0, 0, 255]))
            .await,
        StatusCode::CONFLICT,
        "uploading after finalize must be rejected"
    );

    let build2 = fx.wait_for_terminal(build2_id).await;
    assert_eq!(build2["status"].as_str(), Some("changes_detected"));
    assert_eq!(
        counts(&build2),
        (3, 1, 1, 0, 1),
        "expected unchanged:1 changed:1 added:1"
    );
    assert_eq!(
        build2["content_hash_skipped_count"].as_i64(),
        Some(1),
        "only the byte-identical screenshot skips decode/diff"
    );

    let cmps = fx.comparisons(build2_id).await;
    assert_eq!(cmps.len(), 3);

    let about = find_comparison(&cmps, "about");
    assert_eq!(about["status"], "unchanged");
    assert_eq!(
        about["review_status"], "approved",
        "unchanged comparisons are auto-approved"
    );
    assert_eq!(about["has_diff_image"], false);

    let pricing = find_comparison(&cmps, "pricing");
    assert_eq!(pricing["status"], "added");
    assert_eq!(pricing["review_status"], "pending");

    let home = find_comparison(&cmps, "home");
    assert_eq!(home["status"], "changed");
    assert_eq!(home["review_status"], "pending");
    assert_eq!(home["has_diff_image"], true);
    assert!(
        home["diff_ratio"].as_f64().unwrap_or(0.0) > 0.0,
        "changed comparison must report a positive diff ratio"
    );
    assert!(
        home["diff_pixel_count"].as_i64().unwrap_or(0) > 0,
        "changed comparison must report diff pixels"
    );

    // 差分画像が PNG として配信される
    let home_id = home["id"].as_str().expect("comparison id");
    let res = fx
        .app
        .get(&format!("/v1/comparisons/{home_id}/diff-content"))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "diff-content");
    assert_eq!(
        res.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/png")
    );
    let diff_bytes = res.bytes().await.expect("diff bytes");
    assert_eq!(
        &diff_bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        "diff image must be a real PNG"
    );

    // スクリーンショット実体も配信される
    let screenshot_id = home["screenshot_id"].as_str().expect("screenshot id");
    let res = fx
        .app
        .get(&format!("/v1/screenshots/{screenshot_id}/content"))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "screenshot content");

    // baseline エントリの実体も配信される
    let entry_id = home["baseline_entry_id"]
        .as_str()
        .expect("baseline entry id");
    let res = fx
        .app
        .get(&format!("/v1/baseline-entries/{entry_id}/content"))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "baseline entry content");

    // ── 個別レビュー → force 無しで承認できる ───────────────────────────
    for name in ["home", "pricing"] {
        let cmp = find_comparison(&cmps, name);
        let id = cmp["id"].as_str().expect("comparison id");
        let res = fx
            .app
            .post_json(
                &format!("/v1/comparisons/{id}/review"),
                json!({ "action": "approve" }),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK, "review {name}");
        let reviewed: Value = res.json().await.expect("reviewed json");
        assert_eq!(reviewed["review_status"], "approved");
    }

    let res = fx.approve(build2_id, false).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "approve without force once everything is reviewed"
    );

    assert_eq!(
        baseline_entry_count(&fx).await,
        3,
        "new baseline holds all three screenshots"
    );

    // ── ビルド #3: #2 と同一 → passed ───────────────────────────────────
    let build3 = fx.create_build("main", "cccc3333").await;
    let build3_id = build_id_of(&build3);

    let home_v2_again = png_with_rect(
        40,
        30,
        [255, 255, 255, 255],
        (2, 2, 10, 10),
        [255, 0, 0, 255],
    );
    assert_eq!(
        fx.upload(build3_id, "home", home_v2_again).await,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.upload(build3_id, "about", about_v1).await,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.upload(build3_id, "pricing", png(20, 20, [10, 200, 10, 255]))
            .await,
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build3_id).await, StatusCode::OK);

    let build3 = fx.wait_for_terminal(build3_id).await;
    assert_eq!(
        build3["status"].as_str(),
        Some("passed"),
        "identical build needs no review"
    );
    assert_eq!(counts(&build3), (3, 0, 0, 0, 3));
    assert_eq!(
        build3["content_hash_skipped_count"].as_i64(),
        Some(3),
        "all byte-identical screenshots skip decode/diff"
    );
    let logs = fx.build_logs(build3_id).await;
    assert!(
        logs.iter().any(|entry| {
            entry["message"]
                .as_str()
                .is_some_and(|message| message.contains("content_hash_skipped 3"))
        }),
        "build log records how many comparisons skipped decode/diff"
    );

    let cmps = fx.comparisons(build3_id).await;
    assert!(cmps.iter().all(|c| c["status"] == "unchanged"));
    assert!(cmps.iter().all(|c| c["review_status"] == "approved"));

    // ビルド一覧（新しい順）
    let res = fx
        .app
        .get(&format!("/v1/projects/{}/builds", fx.project_id))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let list: Value = res.json().await.expect("list json");
    assert_eq!(list["total"].as_u64(), Some(3));
    let numbers: Vec<i64> = list["builds"]
        .as_array()
        .expect("builds array")
        .iter()
        .map(|b| b["number"].as_i64().unwrap_or(-1))
        .collect();
    assert_eq!(numbers, vec![3, 2, 1], "builds are listed newest first");
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_fast_path_rejects_dimension_readable_but_corrupt_stored_png() {
    let fx = setup().await;
    let original = png(12, 12, [20, 40, 60, 255]);
    let baseline_build = fx.create_build("main", "baseline-corrupt-test").await;
    let baseline_build_id = build_id_of(&baseline_build);
    assert_eq!(
        fx.upload(baseline_build_id, "card", original.clone()).await,
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(baseline_build_id).await, StatusCode::OK);
    assert_eq!(
        fx.wait_for_terminal(baseline_build_id).await["status"],
        "changes_detected"
    );
    assert_eq!(
        fx.approve(baseline_build_id, true).await.status(),
        StatusCode::OK
    );

    let build = fx.create_build("main", "corrupt-current").await;
    let build_id = build_id_of(&build);
    assert_eq!(
        fx.upload(build_id, "card", original.clone()).await,
        StatusCode::CREATED
    );
    let shot = entity::screenshots::Entity::find()
        .filter(entity::screenshots::Column::BuildId.eq(build_id))
        .one(&fx.app.state.db)
        .await
        .expect("query shot")
        .expect("shot");
    let baseline = entity::baselines::Entity::find()
        .filter(entity::baselines::Column::SourceBuildId.eq(baseline_build_id))
        .one(&fx.app.state.db)
        .await
        .expect("query baseline")
        .expect("baseline");
    let entry = entity::baseline_entries::Entity::find()
        .filter(entity::baseline_entries::Column::BaselineId.eq(baseline.id))
        .filter(entity::baseline_entries::Column::Name.eq("card"))
        .one(&fx.app.state.db)
        .await
        .expect("query baseline entry")
        .expect("baseline entry");
    assert!(
        service::screenshots::content_hashes_match(
            shot.content_hash.as_deref(),
            entry.content_hash.as_deref()
        ),
        "positive control: the pre-fix metadata-only fast path would pass"
    );

    let corrupt = corrupt_idat(original);
    assert!(
        service::screenshots::validate_png(&corrupt).is_ok(),
        "dimensions remain readable"
    );
    assert!(
        image::load_from_memory(&corrupt).is_err(),
        "full decode detects broken IDAT"
    );
    service::screenshots::upload_png(&fx.app.state.storage, &shot.storage_key, corrupt.into())
        .await
        .expect("replace fixture object with corrupt bytes");

    assert_eq!(fx.finalize(build_id).await, StatusCode::OK);
    let failed = fx.wait_for_terminal(build_id).await;
    assert_eq!(
        failed["status"], "failed",
        "corrupt object must not become passed"
    );
    assert_eq!(failed["content_hash_skipped_count"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_fast_path_rejects_missing_stored_object() {
    let fx = setup().await;
    let original = png(12, 12, [80, 100, 120, 255]);
    let baseline_build = fx.create_build("main", "baseline-missing-test").await;
    let baseline_build_id = build_id_of(&baseline_build);
    assert_eq!(
        fx.upload(baseline_build_id, "card", original.clone()).await,
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(baseline_build_id).await, StatusCode::OK);
    fx.wait_for_terminal(baseline_build_id).await;
    assert_eq!(
        fx.approve(baseline_build_id, true).await.status(),
        StatusCode::OK
    );

    let build = fx.create_build("main", "missing-current").await;
    let build_id = build_id_of(&build);
    assert_eq!(
        fx.upload(build_id, "card", original).await,
        StatusCode::CREATED
    );
    let shot = entity::screenshots::Entity::find()
        .filter(entity::screenshots::Column::BuildId.eq(build_id))
        .one(&fx.app.state.db)
        .await
        .expect("query shot")
        .expect("shot");
    let baseline = entity::baselines::Entity::find()
        .filter(entity::baselines::Column::SourceBuildId.eq(baseline_build_id))
        .one(&fx.app.state.db)
        .await
        .expect("query baseline")
        .expect("baseline");
    let entry = entity::baseline_entries::Entity::find()
        .filter(entity::baseline_entries::Column::BaselineId.eq(baseline.id))
        .filter(entity::baseline_entries::Column::Name.eq("card"))
        .one(&fx.app.state.db)
        .await
        .expect("query baseline entry")
        .expect("baseline entry");
    assert!(
        service::screenshots::content_hashes_match(
            shot.content_hash.as_deref(),
            entry.content_hash.as_deref()
        ),
        "positive control: the pre-fix metadata-only fast path would pass"
    );
    fx.app
        .state
        .storage
        .delete(&shot.storage_key)
        .await
        .expect("delete fixture object");

    assert_eq!(fx.finalize(build_id).await, StatusCode::OK);
    let failed = fx.wait_for_terminal(build_id).await;
    assert_eq!(
        failed["status"], "failed",
        "missing object must not become passed"
    );
    assert_eq!(failed["content_hash_skipped_count"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_mismatch_rejects_pixel_identical_replacement_png() {
    let fx = setup().await;
    let original = png(12, 12, [20, 40, 60, 255]);
    let baseline_build = fx.create_build("main", "baseline-swap-test").await;
    let baseline_build_id = build_id_of(&baseline_build);
    assert_eq!(
        fx.upload(baseline_build_id, "card", original.clone()).await,
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(baseline_build_id).await, StatusCode::OK);
    assert_eq!(
        fx.wait_for_terminal(baseline_build_id).await["status"],
        "changes_detected"
    );
    assert_eq!(
        fx.approve(baseline_build_id, true).await.status(),
        StatusCode::OK
    );

    let build = fx.create_build("main", "swap-current").await;
    let build_id = build_id_of(&build);
    assert_eq!(
        fx.upload(build_id, "card", original.clone()).await,
        StatusCode::CREATED
    );
    let shot = entity::screenshots::Entity::find()
        .filter(entity::screenshots::Column::BuildId.eq(build_id))
        .one(&fx.app.state.db)
        .await
        .expect("query shot")
        .expect("shot");
    let baseline = entity::baselines::Entity::find()
        .filter(entity::baselines::Column::SourceBuildId.eq(baseline_build_id))
        .one(&fx.app.state.db)
        .await
        .expect("query baseline")
        .expect("baseline");
    let entry = entity::baseline_entries::Entity::find()
        .filter(entity::baseline_entries::Column::BaselineId.eq(baseline.id))
        .filter(entity::baseline_entries::Column::Name.eq("card"))
        .one(&fx.app.state.db)
        .await
        .expect("query baseline entry")
        .expect("baseline entry");
    assert!(
        service::screenshots::content_hashes_match(
            shot.content_hash.as_deref(),
            entry.content_hash.as_deref(),
        ),
        "positive control: DB hashes match before replacement"
    );

    let reencoded = inject_text_chunk(original);
    assert!(
        image::load_from_memory(&reencoded).is_ok(),
        "replacement PNG is valid"
    );
    service::screenshots::upload_png(&fx.app.state.storage, &shot.storage_key, reencoded.into())
        .await
        .expect("replace fixture object with pixel-identical but byte-different PNG");

    assert_eq!(fx.finalize(build_id).await, StatusCode::OK);
    let failed = fx.wait_for_terminal(build_id).await;
    assert_eq!(
        failed["status"], "failed",
        "pixel-identical but byte-different replacement must not become passed"
    );
    assert_eq!(failed["content_hash_skipped_count"], 0);
}

/// このプロジェクトの最新 baseline のエントリ数。
async fn baseline_entry_count(fx: &Fixture) -> usize {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

    let baseline = entity::baselines::Entity::find()
        .filter(entity::baselines::Column::ProjectId.eq(fx.project_id))
        .order_by_desc(entity::baselines::Column::CreatedAt)
        .order_by_desc(entity::baselines::Column::Id)
        .one(&fx.app.state.db)
        .await
        .expect("query baseline")
        .expect("baseline exists");

    entity::baseline_entries::Entity::find()
        .filter(entity::baseline_entries::Column::BaselineId.eq(baseline.id))
        .all(&fx.app.state.db)
        .await
        .expect("query entries")
        .len()
}

// ── ネガティブケース ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn pat_without_write_build_scope_cannot_create_build() {
    let fx = setup().await;
    let user = fx
        .app
        .find_user_by_username(&fx.app.provider.username)
        .await
        .expect("user");

    // read:build しか持たない PAT
    let (weak_token, _) = fx
        .app
        .insert_personal_token(user.id, vec![Scope::ReadBuild])
        .await;

    let res = fx
        .app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                fx.tenant_slug, fx.project_slug
            ),
            &weak_token,
            json!({ "branch": "main", "commit_sha": "deadbeef" }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "read:build must not allow build creation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_member_pat_cannot_create_build_in_foreign_tenant() {
    let owner = setup().await;

    // 別のユーザー（別 TestApp = 別 OAuth ユーザー）の PAT
    let outsider_app = TestApp::new().await;
    let outsider = outsider_app.login_as_new_user().await;
    let (outsider_token, _) = outsider_app
        .insert_personal_token(outsider.id, vec![Scope::WriteBuild, Scope::ReadBuild])
        .await;

    let res = outsider_app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                owner.tenant_slug, owner.project_slug
            ),
            &outsider_token,
            json!({ "branch": "main", "commit_sha": "deadbeef" }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "non-members must not create builds in a foreign tenant"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_screenshot_name_is_conflict() {
    let fx = setup().await;
    let build = fx.create_build("main", "dup00001").await;
    let build_id = build_id_of(&build);

    assert_eq!(
        fx.upload(build_id, "home", png(8, 8, [1, 2, 3, 255])).await,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.upload(build_id, "home", png(8, 8, [4, 5, 6, 255])).await,
        StatusCode::CONFLICT,
        "duplicate screenshot name in one build must be rejected"
    );
}

/// 2MB を超える PNG が受け付けられる（axum の既定ボディ上限の回帰テスト）。
///
/// `DefaultBodyLimit` を 25MB に上げていないと、`Multipart` が 2MB で読み込みを
/// 打ち切って 400（`Error parsing multipart/form-data request`）になる。
/// 実機のフルページスクリーンショットは平気で 2MB を超える。
#[tokio::test(flavor = "multi_thread")]
async fn screenshots_larger_than_the_default_body_limit_are_accepted() {
    let fx = setup().await;
    let build = fx.create_build("main", "bigpng01").await;
    let build_id = build_id_of(&build);

    let png = noise_png(1024, 1024);
    assert!(
        png.len() > 2 * 1024 * 1024,
        "fixture must exceed axum's 2MB default body limit, got {} bytes",
        png.len()
    );

    assert_eq!(
        fx.upload(build_id, "home", png).await,
        StatusCode::CREATED,
        "multi-megabyte screenshot upload"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_png_upload_is_rejected() {
    let fx = setup().await;
    let build = fx.create_build("main", "notpng01").await;
    let build_id = build_id_of(&build);

    assert_eq!(
        fx.upload(build_id, "home", b"this is definitely not a png".to_vec())
            .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn finalize_twice_is_conflict() {
    let fx = setup().await;
    let build = fx.create_build("main", "twice001").await;
    let build_id = build_id_of(&build);

    assert_eq!(
        fx.upload(build_id, "home", png(8, 8, [1, 2, 3, 255])).await,
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_id).await, StatusCode::OK);
    assert_eq!(
        fx.finalize(build_id).await,
        StatusCode::CONFLICT,
        "finalizing an already finalized build must be rejected"
    );
}

/// `GET /v1/projects/{project_id}/builds/{number}` — UI の URL がビルド番号ベースなので、
/// 一覧を舐めずに 1 件引けることと、認可が ID 直参照と揃っていることを確認する。
#[tokio::test(flavor = "multi_thread")]
async fn build_can_be_fetched_by_project_scoped_number() {
    let fx = setup().await;
    let project_id = fx.project_id;

    let first = fx.create_build("main", "aaaaaaaaaaaa").await;
    let second = fx.create_build("feature", "bbbbbbbbbbbb").await;
    assert_eq!(first["number"].as_i64(), Some(1));
    assert_eq!(second["number"].as_i64(), Some(2));

    // 番号で引くと ID 直参照と同じビルドが返る。
    let res = fx
        .app
        .get(&format!("/v1/projects/{project_id}/builds/2"))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.expect("build json");
    assert_eq!(body["id"], second["id"]);
    assert_eq!(body["number"].as_i64(), Some(2));
    assert_eq!(body["branch"].as_str(), Some("feature"));

    // 存在しない番号は 404（プロジェクトへのアクセス権はあるので隠す必要がない）。
    assert_eq!(
        fx.app
            .get(&format!("/v1/projects/{project_id}/builds/9999"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    // read:build を持つ PAT でも引ける。
    assert_eq!(
        fx.app
            .get_with_bearer(&format!("/v1/projects/{project_id}/builds/1"), &fx.token)
            .await
            .status(),
        StatusCode::OK
    );

    // 未認証は 401。
    let anonymous = reqwest::Client::new();
    let res = anonymous
        .get(format!(
            "{}/v1/projects/{project_id}/builds/1",
            fx.app.base_url()
        ))
        .send()
        .await
        .expect("anonymous request");
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 他テナントのユーザーからは 403（プロジェクトの存在を漏らさない）。
    let outsider = TestApp::new().await;
    outsider.login_as_new_user().await;
    assert_eq!(
        outsider
            .get(&format!("/v1/projects/{project_id}/builds/1"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

/// 失敗した screenshots モードのビルドの再実行は、アップロード済み PNG の
/// **比較から**やり直して完走する（storybook モードと違いレンダリングは無い）。
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_screenshots_build_can_be_retried_from_the_compare_step() {
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};

    let fx = setup().await;

    let build = fx.create_build("main", "retrycmp1").await;
    let build_id: Uuid = build["id"].as_str().expect("build id").parse().unwrap();
    assert_eq!(
        fx.upload(build_id, "home", png(64, 64, [200, 30, 30, 255]))
            .await,
        StatusCode::CREATED
    );
    assert_eq!(fx.finalize(build_id).await, StatusCode::OK);
    let done = fx.wait_for_terminal(build_id).await;
    assert_eq!(done["status"].as_str(), Some("changes_detected"));

    // 比較ジョブが一時障害で落ちた状況を DB 直接更新で再現する
    // （compare の失敗を API 経由で決定的に起こす口は無い）。
    let model = entity::builds::Entity::find_by_id(build_id)
        .one(&fx.app.state.db)
        .await
        .expect("load build")
        .expect("build row");
    let mut active: entity::builds::ActiveModel = model.into();
    active.status = Set(entity::builds::BuildStatus::Failed);
    active.error_message = Set(Some("simulated compare failure".into()));
    active.update(&fx.app.state.db).await.expect("force failed");

    // 再実行 → queued に戻り、worker が取得して比較から完走する。
    let res = fx
        .app
        .post_json(&format!("/v1/builds/{build_id}/retry"), json!({}))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "retry failed build");
    let retried: Value = res.json().await.expect("retry json");
    assert_eq!(
        retried["status"].as_str(),
        Some("queued"),
        "screenshots-mode retry waits for the compare worker"
    );
    assert!(retried["error_message"].is_null());

    let done = fx.wait_for_terminal(build_id).await;
    assert_eq!(
        done["status"].as_str(),
        Some("changes_detected"),
        "retried build completes (error: {:?})",
        done["error_message"]
    );
    // 比較は作り直されて重複しない（compare ジョブが開始時に前回分を捨てる）。
    let comparisons = fx.comparisons(build_id).await;
    assert_eq!(comparisons.len(), 1);
    assert_eq!(comparisons[0]["status"].as_str(), Some("added"));
}
