//! 承認経路の偽陰性ガードの統合テスト。
//!
//! `service::builds::approve_build` は長らく「レビュー結果を見ずに、そのビルドの
//! スクリーンショット集合をそのまま baseline へ昇格する」実装だった。そのため
//! 正常操作だけで次の 3 つが起きた。ここではそれぞれを本物の Postgres +
//! ストレージ + 比較ジョブに対して再現し、承認が止まることを確認する。
//!
//! 1. 却下した比較が baseline に焼き付く
//! 2. 古いビルドを後追い承認すると baseline が巻き戻る
//! 3. `force` の一括承認が story の消滅まで飲み込む

mod common;

use std::time::Duration;

use common::TestApp;
use entity::scopes::Scope;
use image::{Rgba, RgbaImage};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const POLL_TIMEOUT: Duration = Duration::from_secs(120);

fn png(color: [u8; 4]) -> Vec<u8> {
    let image = RgbaImage::from_pixel(8, 8, Rgba(color));
    let mut buf = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}

const RED: [u8; 4] = [220, 30, 30, 255];
const BLUE: [u8; 4] = [30, 30, 220, 255];

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
    let tenant_slug = format!("guard-{suffix}");
    let project_slug = format!("web-{suffix}");

    let res = app
        .post_json(
            "/v1/tenants",
            json!({ "name": "Guard Co", "slug": tenant_slug }),
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
    /// スクリーンショットを上げて finalize し、終端状態まで待つ。
    async fn run_build(&self, sha: &str, shots: &[(&str, [u8; 4])]) -> Value {
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
        assert_eq!(res.status(), StatusCode::CREATED, "create build {sha}");
        let build: Value = res.json().await.expect("build json");
        let build_id = build_id_of(&build);

        for (name, color) in shots {
            let status = self
                .app
                .upload_screenshot(build_id, &self.token, name, png(*color))
                .await
                .status();
            assert_eq!(status, StatusCode::CREATED, "upload {name}");
        }

        let status = self
            .app
            .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &self.token)
            .await
            .status();
        assert_eq!(status, StatusCode::OK, "finalize {sha}");

        self.wait_for_terminal(build_id).await
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
            if !matches!(status.as_str(), "pending" | "processing" | "rendering") {
                return build;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "build {build_id} stuck in {status}"
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
        body["comparisons"].as_array().expect("array").clone()
    }

    async fn review(&self, build_id: Uuid, name: &str, action: &str) {
        let list = self.comparisons(build_id).await;
        let target = list
            .iter()
            .find(|c| c["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("comparison {name} not found"));
        let comparison_id = target["id"].as_str().expect("comparison id");
        let res = self
            .app
            .post_json(
                &format!("/v1/comparisons/{comparison_id}/review"),
                json!({ "action": action }),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK, "review {name} as {action}");
    }

    async fn approve(&self, build_id: Uuid, body: Value) -> reqwest::Response {
        self.app
            .post_json(&format!("/v1/builds/{build_id}/approve"), body)
            .await
    }

    /// `main` ブランチの現行 baseline の (id, エントリ名一覧)。
    async fn current_baseline(&self) -> Option<(Uuid, Vec<String>)> {
        let baseline =
            service::baselines::latest_on_branch(&self.app.state.db, self.project_id, "main")
                .await
                .expect("latest baseline")?;
        let names = service::baselines::entries(&self.app.state.db, baseline.id)
            .await
            .expect("baseline entries")
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        Some((baseline.id, names))
    }

    async fn build_status(&self, build_id: Uuid) -> String {
        let res = self.app.get(&format!("/v1/builds/{build_id}")).await;
        assert_eq!(res.status(), StatusCode::OK, "get build");
        let build: Value = res.json().await.expect("build json");
        build["status"].as_str().expect("status").to_string()
    }
}

fn build_id_of(build: &Value) -> Uuid {
    build["id"].as_str().expect("build id").parse().unwrap()
}

async fn error_message(res: reqwest::Response) -> String {
    let body: Value = res.json().await.expect("error json");
    body["message"].as_str().unwrap_or_default().to_string()
}

// ── 穴①: 却下した比較が baseline に焼き付く ─────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn rejected_comparison_blocks_approval_and_keeps_the_baseline() {
    let fx = setup().await;

    // 初回ビルドは baseline が無いので全部 added。force で承認して baseline を作る。
    let first = fx.run_build("sha1", &[("home", RED), ("login", RED)]).await;
    assert_eq!(first["status"], "changes_detected");
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");
    let (baseline_id, names) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(names, vec!["home".to_string(), "login".to_string()]);

    // login だけ変わったビルド。login を却下する。
    let second = fx
        .run_build("sha2", &[("home", RED), ("login", BLUE)])
        .await;
    assert_eq!(second["status"], "changes_detected");
    let second_id = build_id_of(&second);
    fx.review(second_id, "login", "reject").await;

    // 却下が残ったまま承認しようとすると 409。
    let res = fx.approve(second_id, json!({})).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "却下された比較があるので承認できない"
    );
    assert!(
        error_message(res).await.contains("login"),
        "どの比較で止まったか分かるメッセージであること"
    );

    // force でも通らない（却下は一括承認の対象外）。
    let res = fx.approve(second_id, json!({ "force": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "force でも却下は覆らない"
    );

    // baseline は 1 つ目のまま。却下した BLUE の login は焼き付いていない。
    let (still, names) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(still, baseline_id, "baseline は進んでいない");
    assert_eq!(names, vec!["home".to_string(), "login".to_string()]);
    assert_eq!(fx.build_status(second_id).await, "changes_detected");
}

// ── 穴②: 古いビルドの後追い承認で baseline が巻き戻る ───────────────────

#[tokio::test(flavor = "multi_thread")]
async fn approving_a_stale_build_after_a_newer_one_is_rejected() {
    let fx = setup().await;

    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");

    // 2 本とも同じ baseline に対して比較される。
    let second = fx.run_build("sha2", &[("home", BLUE)]).await;
    let third = fx.run_build("sha3", &[("home", BLUE)]).await;
    let second_id = build_id_of(&second);
    let third_id = build_id_of(&third);
    assert_eq!(second["status"], "changes_detected");
    assert_eq!(third["status"], "changes_detected");

    // 新しい方を先に承認する。
    let res = fx.approve(third_id, json!({ "force": true })).await;
    assert_eq!(res.status(), StatusCode::OK, "approve newest build");
    let (after_third, _) = fx.current_baseline().await.expect("baseline exists");

    // 古い方を後追いで承認しようとすると 409。
    let res = fx.approve(second_id, json!({ "force": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "古いビルドの後追い承認で baseline を巻き戻せない"
    );
    let message = error_message(res).await;
    assert!(
        message.contains("older") || message.contains("baseline moved"),
        "巻き戻りだと分かるメッセージであること: {message}"
    );

    // baseline は新しい方のまま。
    let (still, _) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(still, after_third, "baseline は巻き戻っていない");
}

// ── 穴③: force が story の消滅まで一括承認する ──────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn force_does_not_silently_approve_story_removals() {
    let fx = setup().await;

    let first = fx
        .run_build("sha1", &[("home", RED), ("legacy", RED)])
        .await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");

    // legacy が撮れなかったビルド（削除なのか撮影漏れなのかは区別できない）。
    let second = fx.run_build("sha2", &[("home", RED)]).await;
    let second_id = build_id_of(&second);
    assert_eq!(second["status"], "changes_detected");
    assert_eq!(second["removed_count"], 1);

    // force だけでは消滅を飲み込まない。
    let res = fx.approve(second_id, json!({ "force": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "force は removed を一括承認しない"
    );
    assert!(
        error_message(res).await.contains("legacy"),
        "消える story 名が示されること"
    );

    // baseline は据え置き。
    let (_, names) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(names, vec!["home".to_string(), "legacy".to_string()]);

    // 明示確認すれば通り、baseline から legacy が落ちる。
    let res = fx
        .approve(second_id, json!({ "force": true, "accept_removals": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "明示確認すれば承認できる");
    let (_, names) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(names, vec!["home".to_string()]);
}

// ── 健全挙動の維持 ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_review_and_approval_still_promotes_the_baseline() {
    let fx = setup().await;

    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");
    let (baseline_id, _) = fx.current_baseline().await.expect("baseline exists");

    // 変更 + 新規。どちらも人手で承認する。
    let second = fx
        .run_build("sha2", &[("home", BLUE), ("about", RED)])
        .await;
    let second_id = build_id_of(&second);
    assert_eq!(second["status"], "changes_detected");
    fx.review(second_id, "home", "approve").await;
    fx.review(second_id, "about", "approve").await;

    let res = fx.approve(second_id, json!({})).await;
    assert_eq!(res.status(), StatusCode::OK, "全レビュー済みなら承認できる");

    let (next, names) = fx.current_baseline().await.expect("baseline exists");
    assert_ne!(next, baseline_id, "baseline が進む");
    assert_eq!(names, vec!["about".to_string(), "home".to_string()]);
    assert_eq!(fx.build_status(second_id).await, "approved");
}
