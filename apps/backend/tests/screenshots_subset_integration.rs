//! screenshots モードの部分アップロード（carry-forward）と baseline 固定の統合テスト。
//!
//! 検証する契約:
//!
//! 1. finalize で `captured_names` を宣言した部分アップロードでは、宣言外の
//!    baseline エントリが `removed` ではなく前回 baseline の流用（unchanged）になり、
//!    承認しても baseline から消えない
//! 2. 「宣言 == 実際のアップロード」が成立しない finalize は 400 で拒否される
//! 3. 比較に使う baseline はビルド作成時に固定され、作成後に別ビルドが承認されて
//!    最新 baseline が動いても比較対象はずれない（`expected_baseline_commit_sha` 照合含む）

mod common;

use std::time::Duration;

use common::TestApp;
use entity::scopes::Scope;
use image::{Rgba, RgbaImage};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const POLL_TIMEOUT: Duration = Duration::from_secs(60);

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let image = RgbaImage::from_pixel(width, height, Rgba(color));
    let mut buf = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}

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
    let tenant_slug = format!("cf-{suffix}");
    let project_slug = format!("web-{suffix}");

    let res = app
        .post_json(
            "/v1/tenants",
            json!({ "name": "CF Co", "slug": tenant_slug }),
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
        .insert_personal_token(user.id, vec![Scope::WriteBuild, Scope::ReadBuild])
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
    async fn create_build(&self, sha: &str) -> Value {
        let res = self
            .app
            .post_json_with_bearer(
                &format!(
                    "/v1/ci/projects/{}/{}/builds",
                    self.tenant_slug, self.project_slug
                ),
                &self.token,
                json!({ "branch": "main", "commit_sha": sha }),
            )
            .await;
        assert_eq!(res.status(), StatusCode::CREATED, "create build");
        res.json().await.expect("build json")
    }

    async fn upload(&self, build_id: Uuid, name: &str, png: Vec<u8>) {
        let status = self
            .app
            .upload_screenshot(build_id, &self.token, name, png)
            .await
            .status();
        assert_eq!(status, StatusCode::CREATED, "upload {name}");
    }

    async fn finalize(&self, build_id: Uuid, body: Option<Value>) -> reqwest::Response {
        match body {
            Some(body) => {
                self.app
                    .post_json_with_bearer(
                        &format!("/v1/ci/builds/{build_id}/finalize"),
                        &self.token,
                        body,
                    )
                    .await
            }
            None => {
                self.app
                    .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &self.token)
                    .await
            }
        }
    }

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
            if !matches!(status.as_str(), "pending" | "processing") {
                return build;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "build {build_id} stuck in {status} after {POLL_TIMEOUT:?}"
            );
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

    async fn approve_force(&self, build_id: Uuid) {
        let res = self
            .app
            .post_json(
                &format!("/v1/builds/{build_id}/approve"),
                json!({ "force": true }),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK, "force approve");
    }

    /// 最新 baseline とそのエントリ名一覧。
    async fn latest_baseline(&self) -> (Uuid, Vec<String>) {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let baseline = entity::baselines::Entity::find()
            .filter(entity::baselines::Column::ProjectId.eq(self.project_id))
            .order_by_desc(entity::baselines::Column::CreatedAt)
            .order_by_desc(entity::baselines::Column::Id)
            .one(&self.app.state.db)
            .await
            .expect("query baseline")
            .expect("baseline exists");

        let names = entity::baseline_entries::Entity::find()
            .filter(entity::baseline_entries::Column::BaselineId.eq(baseline.id))
            .order_by_asc(entity::baseline_entries::Column::Name)
            .all(&self.app.state.db)
            .await
            .expect("query entries")
            .into_iter()
            .map(|e| e.name)
            .collect();

        (baseline.id, names)
    }

    /// home / about / pricing の 3 枚で全撮影ビルドを作り、承認して baseline を確立する。
    async fn establish_baseline(&self, sha: &str, home: Vec<u8>) -> Uuid {
        let build = self.create_build(sha).await;
        let build_id: Uuid = build["id"].as_str().expect("id").parse().unwrap();
        self.upload(build_id, "home", home).await;
        self.upload(build_id, "about", png(30, 30, [200, 200, 200, 255]))
            .await;
        self.upload(build_id, "pricing", png(20, 20, [10, 200, 10, 255]))
            .await;
        assert_eq!(self.finalize(build_id, None).await.status(), StatusCode::OK);
        self.wait_for_terminal(build_id).await;
        self.approve_force(build_id).await;
        build_id
    }
}

fn build_id_of(build: &Value) -> Uuid {
    build["id"].as_str().expect("build id").parse().unwrap()
}

fn find_comparison<'a>(list: &'a [Value], name: &str) -> &'a Value {
    list.iter()
        .find(|c| c["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("comparison {name} not found"))
}

/// 部分アップロード: 宣言外の baseline エントリは removed にならず流用され、
/// 承認後の baseline からも消えない。
///
/// positive control: carry-forward の無い旧実装では about / pricing が `removed`
/// になり（removed_count = 2）、承認で baseline が 1 件に縮む → このテストは落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn subset_upload_carries_forward_unselected_baseline_entries() {
    let fx = setup().await;
    let home_v1 = png(40, 30, [255, 255, 255, 255]);
    fx.establish_baseline("base0001", home_v1).await;

    // home だけ撮り直す部分アップロード（内容も変える）。
    let build = fx.create_build("subset01").await;
    let build_id = build_id_of(&build);
    fx.upload(build_id, "home", png(40, 30, [255, 0, 0, 255]))
        .await;
    let res = fx
        .finalize(build_id, Some(json!({ "captured_names": ["home"] })))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "subset finalize");

    let build = fx.wait_for_terminal(build_id).await;
    assert_eq!(build["status"].as_str(), Some("changes_detected"));
    assert_eq!(build["total_count"].as_i64(), Some(3));
    assert_eq!(build["changed_count"].as_i64(), Some(1), "home changed");
    assert_eq!(
        build["removed_count"].as_i64(),
        Some(0),
        "unselected baseline entries must be carried forward, not reported as removed"
    );
    assert_eq!(build["unchanged_count"].as_i64(), Some(2));

    let cmps = fx.comparisons(build_id).await;
    for name in ["about", "pricing"] {
        let cmp = find_comparison(&cmps, name);
        assert_eq!(cmp["status"], "unchanged", "{name} is carried forward");
        assert!(
            cmp["screenshot_id"].as_str().is_some(),
            "carry-forward materializes a screenshot for {name} \
             (so approval keeps it in the next baseline)"
        );
    }

    // 承認しても宣言外のエントリは baseline に残る（消滅しない）。
    fx.approve_force(build_id).await;
    let (_, names) = fx.latest_baseline().await;
    assert_eq!(
        names,
        vec!["about", "home", "pricing"],
        "the promoted baseline keeps carried-forward entries"
    );
}

/// 何も撮らない部分アップロード（`captured_names: []`）は全エントリ流用で passed になる。
#[tokio::test(flavor = "multi_thread")]
async fn empty_captured_set_reuses_the_whole_baseline() {
    let fx = setup().await;
    fx.establish_baseline("base0002", png(40, 30, [255, 255, 255, 255]))
        .await;

    let build = fx.create_build("subset02").await;
    let build_id = build_id_of(&build);
    let res = fx
        .finalize(build_id, Some(json!({ "captured_names": [] })))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "empty subset finalize");

    let build = fx.wait_for_terminal(build_id).await;
    assert_eq!(build["status"].as_str(), Some("passed"));
    assert_eq!(build["total_count"].as_i64(), Some(3));
    assert_eq!(build["unchanged_count"].as_i64(), Some(3));
    assert_eq!(build["removed_count"].as_i64(), Some(0));
}

/// 宣言とアップロードの不一致（欠落・過剰）は finalize が 400 で拒否する。
#[tokio::test(flavor = "multi_thread")]
async fn mismatched_captured_names_are_rejected() {
    let fx = setup().await;
    fx.establish_baseline("base0003", png(40, 30, [255, 255, 255, 255]))
        .await;

    // 宣言したのにアップロードが欠けている → 撮影失敗が流用に化けるので拒否。
    let build = fx.create_build("subset03").await;
    let build_id = build_id_of(&build);
    fx.upload(build_id, "home", png(40, 30, [1, 2, 3, 255]))
        .await;
    let res = fx
        .finalize(
            build_id,
            Some(json!({ "captured_names": ["home", "about"] })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "missing upload");

    // アップロードしたのに宣言に無い → 計画と実撮影のずれなので拒否。
    let res = fx
        .finalize(build_id, Some(json!({ "captured_names": [] })))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "undeclared upload");

    // 一致させれば通る。
    let res = fx
        .finalize(build_id, Some(json!({ "captured_names": ["home"] })))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "matching declaration");
}

/// screenshots モードの only_story_ids は引き続き 400（名前ベースの captured_names を使う）。
#[tokio::test(flavor = "multi_thread")]
async fn only_story_ids_is_still_rejected_for_screenshots_mode() {
    let fx = setup().await;
    let build = fx.create_build("subset04").await;
    let build_id = build_id_of(&build);
    fx.upload(build_id, "home", png(8, 8, [1, 2, 3, 255])).await;

    let res = fx
        .finalize(build_id, Some(json!({ "only_story_ids": ["home"] })))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.text().await.expect("body");
    assert!(
        body.contains("captured_names"),
        "the error should point the caller at captured_names: {body}"
    );
}

/// 比較に使う baseline はビルド作成時に固定される。
///
/// 作成後に別ビルドの承認で最新 baseline が動いても、比較は固定した baseline に
/// 対して行われる（クライアントが計画に使った起点とずれない）。
///
/// positive control: 比較時に最新 baseline を引き直す旧実装では、このビルドは
/// 新 baseline（home v2）と比較されて changes_detected になり、このテストは落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn comparison_uses_the_baseline_pinned_at_build_creation() {
    let fx = setup().await;
    let home_v1 = png(40, 30, [255, 255, 255, 255]);
    let first_build_id = fx.establish_baseline("base0004", home_v1.clone()).await;
    let (pinned_baseline_id, _) = fx.latest_baseline().await;

    // このビルドは作成時点の baseline（home v1）に固定される。
    // 作成レスポンスの baseline_commit_sha も固定値を指す。
    let pinned_build = fx.create_build("pinned01").await;
    let pinned_build_id = build_id_of(&pinned_build);
    assert_eq!(
        pinned_build["baseline_commit_sha"].as_str(),
        Some("base0004"),
        "creation response reports the pinned baseline's source commit"
    );
    let _ = first_build_id;

    // 別ビルドが home v2 で承認され、最新 baseline が入れ替わる。
    fx.establish_baseline("moved001", png(40, 30, [0, 0, 255, 255]))
        .await;
    let (latest_baseline_id, _) = fx.latest_baseline().await;
    assert_ne!(pinned_baseline_id, latest_baseline_id, "baseline moved");

    // 固定済みビルドへ v1 と同一の home をアップロード → 固定 baseline と比較して passed。
    fx.upload(pinned_build_id, "home", home_v1).await;
    fx.upload(pinned_build_id, "about", png(30, 30, [200, 200, 200, 255]))
        .await;
    fx.upload(pinned_build_id, "pricing", png(20, 20, [10, 200, 10, 255]))
        .await;
    // 計画の起点（作成時に受け取った baseline）を照合させて finalize する。
    let res = fx
        .finalize(
            pinned_build_id,
            Some(json!({ "expected_baseline_commit_sha": "base0004" })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "expected baseline matches");

    let build = fx.wait_for_terminal(pinned_build_id).await;
    assert_eq!(
        build["status"].as_str(),
        Some("passed"),
        "comparison must run against the pinned baseline (home v1), not the moved one"
    );
    assert_eq!(
        build["baseline_id"].as_str().map(|s| s.parse().unwrap()),
        Some(pinned_baseline_id),
        "the build records the pinned baseline it compared against"
    );
}

/// 計画の起点とビルドの固定 baseline がずれた finalize は 400 で拒否される。
#[tokio::test(flavor = "multi_thread")]
async fn stale_expected_baseline_is_rejected_at_finalize() {
    let fx = setup().await;
    fx.establish_baseline("base0005", png(40, 30, [255, 255, 255, 255]))
        .await;

    // baseline が動いた後に作られたビルドは新 baseline に固定される。
    fx.establish_baseline("moved002", png(40, 30, [0, 255, 0, 255]))
        .await;
    let build = fx.create_build("stale001").await;
    let build_id = build_id_of(&build);
    fx.upload(build_id, "home", png(8, 8, [1, 2, 3, 255])).await;

    // 旧 baseline を起点に計画したと主張する finalize → 固定値と不一致で 400。
    let res = fx
        .finalize(
            build_id,
            Some(json!({ "expected_baseline_commit_sha": "base0005" })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.text().await.expect("body");
    assert!(
        body.contains("expected_baseline_commit_sha"),
        "error should name the mismatched field: {body}"
    );
}
