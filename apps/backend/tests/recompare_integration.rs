//! 再比較（`POST /v1/builds/{id}/recompare`）の統合テスト。
//!
//! レビューを待っている間に別のビルドが承認されると baseline が動き、取り残された
//! ビルドは「baseline moved」で承認できなくなる。撮影済みのスクリーンショットは
//! 残っているので、比較だけやり直せば救える——というのが再比較の役目。
//!
//! ここで確認するのは、救済が効くことと、**救済の入口が広がりすぎていない**こと:
//!
//! 1. baseline が動いたビルドを再比較すると、現行 baseline と比べ直して承認できる
//! 2. baseline が動いていないビルドの再比較は 409（レビューだけ失われるため）
//! 3. finalize の再送を再比較の代わりに使えない（CI トークンでの裏口）
//! 4. `queued` の再投入は、再比較が付けた由来印がある行に限る

mod common;

use std::time::Duration;

use common::TestApp;
use entity::{builds, scopes::Scope};
use image::{Rgba, RgbaImage};
use reqwest::StatusCode;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
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
    token: String,
}

async fn setup() -> Fixture {
    let app = TestApp::new().await;
    let user = app.login_as_new_user().await;

    let suffix = &Uuid::new_v4().to_string()[..8];
    let tenant_slug = format!("recmp-{suffix}");
    let project_slug = format!("web-{suffix}");

    let res = app
        .post_json(
            "/v1/tenants",
            json!({ "name": "Recompare Co", "slug": tenant_slug }),
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
    let _project_id: Uuid = project["id"].as_str().expect("project id").parse().unwrap();

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
            if !matches!(
                status.as_str(),
                "pending" | "queued" | "processing" | "rendering"
            ) {
                return build;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "build {build_id} stuck in {status}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn approve(&self, build_id: Uuid, body: Value) -> reqwest::Response {
        self.app
            .post_json(&format!("/v1/builds/{build_id}/approve"), body)
            .await
    }

    async fn recompare(&self, build_id: Uuid) -> reqwest::Response {
        self.app
            .post_json(&format!("/v1/builds/{build_id}/recompare"), json!({}))
            .await
    }

    async fn finalize(&self, build_id: Uuid) -> reqwest::Response {
        self.app
            .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &self.token)
            .await
    }

    async fn reload(&self, build_id: Uuid) -> builds::Model {
        builds::Entity::find_by_id(build_id)
            .one(&self.app.state.db)
            .await
            .expect("load build")
            .expect("build exists")
    }

    /// ワーカーが動いていると作れない状態（queued で止まったビルド）を直接作る。
    async fn force_queued(&self, build_id: Uuid, marker: Option<chrono::Duration>) {
        let build = self.reload(build_id).await;
        let mut active: builds::ActiveModel = build.into();
        active.status = Set(builds::BuildStatus::Queued);
        active.recompare_requested_at =
            Set(marker.map(|age| chrono::Utc::now().fixed_offset() - age));
        active
            .update(&self.app.state.db)
            .await
            .expect("force build into queued");
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
}

fn build_id_of(build: &Value) -> Uuid {
    build["id"].as_str().expect("build id").parse().unwrap()
}

async fn error_message(res: reqwest::Response) -> String {
    let body: Value = res.json().await.expect("error json");
    body["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// 本命の救済経路。取り残されたビルドが、撮り直さずに承認できるところまで戻る。
#[tokio::test(flavor = "multi_thread")]
async fn recompare_lets_a_stranded_build_be_approved_against_the_new_baseline() {
    let fx = setup().await;

    // baseline を作る。
    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");

    // 同じ差分を持つ 2 本が、どちらも最初の baseline に対して比較される。
    let second = fx.run_build("sha2", &[("home", BLUE)]).await;
    let third = fx.run_build("sha3", &[("home", BLUE)]).await;
    let second_id = build_id_of(&second);
    let third_id = build_id_of(&third);
    assert_eq!(second["status"], "changes_detected");
    assert_eq!(third["status"], "changes_detected");

    // 先に 2 本目を承認すると baseline が動き、3 本目が取り残される。
    let res = fx.approve(second_id, json!({ "force": true })).await;
    assert_eq!(res.status(), StatusCode::OK, "approve the second build");

    let res = fx.approve(third_id, json!({ "force": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "baseline が動いた後の承認は止まる"
    );
    let message = error_message(res).await;
    assert!(
        message.contains("baseline moved"),
        "取り残された理由を伝えること: {message}"
    );

    // 再比較すると、現行 baseline と比べ直す。差分は解消済みなので passed になる。
    let res = fx.recompare(third_id).await;
    assert_eq!(res.status(), StatusCode::OK, "recompare the stranded build");
    let build = fx.wait_for_terminal(third_id).await;
    assert_eq!(
        build["status"], "passed",
        "現行 baseline と同じ絵なので差分は残らない"
    );
    assert_eq!(build["changed_count"], 0);

    // 比較結果は作り直され、承認も通るようになる。
    let names: Vec<String> = fx
        .comparisons(third_id)
        .await
        .into_iter()
        .map(|c| c["status"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, vec!["unchanged"], "新 baseline との比較に置き換わる");

    let res = fx.approve(third_id, json!({})).await;
    assert_eq!(res.status(), StatusCode::OK, "再比較したビルドは承認できる");
}

/// baseline が動いていないなら、やり直しても結果は同じ。レビューだけ失われるので止める。
#[tokio::test(flavor = "multi_thread")]
async fn recompare_is_rejected_when_the_baseline_has_not_moved() {
    let fx = setup().await;

    // baseline がまだ無いビルド（全 added）。current も build.baseline_id も無い。
    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let first_id = build_id_of(&first);
    assert_eq!(first["status"], "changes_detected");

    let res = fx.recompare(first_id).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "baseline が無いビルドの再比較は無意味"
    );
    let message = error_message(res).await;
    assert!(
        message.contains("nothing would change"),
        "結果が変わらないことを伝えること: {message}"
    );

    // baseline を作ったうえで、その baseline に対して比較済みのビルドも同じ。
    let res = fx.approve(first_id, json!({ "force": true })).await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");

    let second = fx.run_build("sha2", &[("home", BLUE)]).await;
    let second_id = build_id_of(&second);
    assert_eq!(second["status"], "changes_detected");

    let res = fx.recompare(second_id).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "現行 baseline と比較済みのビルドは再比較しない"
    );

    // レビュー待ちの比較は消えていない。
    assert_eq!(
        fx.comparisons(second_id).await.len(),
        1,
        "拒否された再比較は比較結果に触らない"
    );
    assert_eq!(
        fx.reload(second_id).await.status,
        builds::BuildStatus::ChangesDetected
    );
}

/// 承認・却下が確定したビルドは再比較しない（レビュー結果を書き換えない）。
#[tokio::test(flavor = "multi_thread")]
async fn recompare_is_rejected_for_builds_whose_review_is_settled() {
    let fx = setup().await;

    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let first_id = build_id_of(&first);
    let res = fx.approve(first_id, json!({ "force": true })).await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");

    // baseline を動かして、無意味実行ガードでは止まらない状態にする。
    let second = fx.run_build("sha2", &[("home", BLUE)]).await;
    let res = fx
        .approve(build_id_of(&second), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve second build");

    let res = fx.recompare(first_id).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "承認済みのビルドは再比較できない"
    );
    let message = error_message(res).await;
    assert!(
        message.contains("changes_detected") || message.contains("passed"),
        "対象になる状態を伝えること: {message}"
    );
}

/// finalize の再送を再比較の代わりに使えないこと。
///
/// `changes_detected` / `passed` から `queued` へ戻れるようになったので、遷移表だけを
/// 頼りにしていると CI トークン（Member 権限）での finalize 再送が、部分撮影の除外も
/// baseline のリセットも通さない再比較として成立してしまう。
#[tokio::test(flavor = "multi_thread")]
async fn finalize_cannot_be_resent_to_recompare_a_finished_build() {
    let fx = setup().await;

    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let first_id = build_id_of(&first);
    assert_eq!(first["status"], "changes_detected");

    let res = fx.finalize(first_id).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "比較が終わったビルドへの finalize 再送は通らない"
    );

    let build = fx.reload(first_id).await;
    assert_eq!(
        build.status,
        builds::BuildStatus::ChangesDetected,
        "拒否された finalize はビルドを queued に戻さない"
    );
    assert!(
        build.recompare_requested_at.is_none(),
        "finalize は再比較の由来印を付けない"
    );
}

/// `queued` の再投入は、再比較が付けた由来印がある行に限ること。
///
/// 由来印なしの `queued`（finalize 直後・retry 直後）を受け付けると、まだ
/// レンダリングしていない storybook ビルドへ比較ジョブを撃ち込めてしまう。
#[tokio::test(flavor = "multi_thread")]
async fn requeueing_is_limited_to_builds_marked_by_a_recompare() {
    let fx = setup().await;

    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let first_id = build_id_of(&first);

    // ワーカーを止めて、queued で止まったビルドを作れるようにする。
    fx.app.stop_workers();
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 由来印なしの queued（= 処理待ちの新しいビルド）は受け付けない。
    fx.force_queued(first_id, None).await;
    let res = fx.recompare(first_id).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "処理待ちのビルドを再比較の入口にしない"
    );
    let message = error_message(res).await;
    assert!(
        message.contains("has not been compared yet"),
        "処理待ちであることを伝えること: {message}"
    );

    // 由来印はあるが、まだキューに入れたばかり（連打）のときも受け付けない。
    fx.force_queued(first_id, Some(chrono::Duration::seconds(5)))
        .await;
    let res = fx.recompare(first_id).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "キュー投入直後の再投入は畳む"
    );
    let message = error_message(res).await;
    assert!(
        message.contains("wait at least"),
        "待つべき時間を伝えること: {message}"
    );

    // 十分に待っても queued のままなら、取りこぼしとして再投入を受け付ける。
    fx.force_queued(first_id, Some(chrono::Duration::seconds(600)))
        .await;
    let res = fx.recompare(first_id).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "queued のまま止まったビルドは再投入で回収できる"
    );
    let build = fx.reload(first_id).await;
    assert_eq!(
        build.status,
        builds::BuildStatus::Queued,
        "再投入は状態を作り直さない"
    );

    // 再投入で待ち時間は測り直される（叩くたびに積めるようにはしない）。
    let res = fx.recompare(first_id).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "再投入の直後はまた待たせる"
    );
}
