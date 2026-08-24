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
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    QueryFilter, Statement, TransactionTrait,
};
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
    async fn create_build(&self, branch: &str, sha: &str, shots: &[(&str, [u8; 4])]) -> Uuid {
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

        build_id
    }

    /// スクリーンショットを上げて finalize し、終端状態まで待つ。
    async fn run_build(&self, sha: &str, shots: &[(&str, [u8; 4])]) -> Value {
        let build_id = self.create_build("main", sha, shots).await;

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

/// PostgreSQL trigger を更新処理の直前で止める deterministic barrier。
///
/// テスト側の transaction が advisory lock を保持し、対象 UPDATE は trigger 内で同じ
/// lock を待つ。これにより sleep の速さに依存せず、レビュー/却下が DB 更新へ入った
/// 瞬間に承認を競合させられる。
async fn install_update_barrier(
    fx: &Fixture,
    table: &str,
    condition: &str,
) -> (sea_orm::DatabaseTransaction, i64) {
    let suffix = Uuid::new_v4().simple().to_string();
    let function = format!("approval_race_{suffix}");
    let trigger = format!("approval_race_trigger_{suffix}");
    let lock_key = i64::from(u32::from_be_bytes(
        Uuid::new_v4().as_bytes()[..4].try_into().unwrap(),
    ));
    fx.app
        .state
        .db
        .execute_unprepared(&format!(
            "CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$\
             BEGIN IF {condition} THEN PERFORM pg_advisory_xact_lock({lock_key}); END IF; \
             RETURN NEW; END $$; \
             CREATE TRIGGER {trigger} BEFORE UPDATE ON {table} FOR EACH ROW \
             EXECUTE FUNCTION {function}();"
        ))
        .await
        .expect("install approval race barrier");

    let barrier = fx.app.state.db.begin().await.expect("begin barrier");
    barrier
        .execute_unprepared(&format!("SELECT pg_advisory_xact_lock({lock_key})"))
        .await
        .expect("hold barrier");
    (barrier, lock_key)
}

async fn wait_for_advisory_waiter(fx: &Fixture, lock_key: i64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row = fx
            .app
            .state
            .db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*)::bigint AS count FROM pg_locks \
                 WHERE locktype = 'advisory' AND NOT granted \
                   AND classid = 0 AND objid = $1 AND objsubid = 1",
                [lock_key.into()],
            ))
            .await
            .expect("query advisory waiters")
            .expect("count row");
        let count: i64 = row.try_get("", "count").expect("waiter count");
        if count > 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the update barrier"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// barrier を握った承認/レビュー側の DB セッションによって別のセッションが
/// ブロックされるまで待つ、sleep 非依存の観測。
///
/// [`install_update_barrier`] の barrier は対象 UPDATE を advisory lock で止める。
/// その UPDATE を実行している backend（= advisory lock を待っている backend）が、
/// 直前に取った build / project 行ロックを保持したまま止まっているので、後続の
/// 添付・finalize はその行ロック待ちに入る。ここではその「barrier 保持側に
/// ブロックされた backend」を `pg_blocking_pids` で検出する。行ロック待ちを
/// 固定 sleep の速さに依存せず観測でき、クエリ本文にも依存しない。
///
/// `task` が行ロック待ちに入る前に完了してしまった場合（＝直列化されなかった
/// 場合）は即座に失敗させる。
async fn wait_for_row_lock_waiter<T>(
    fx: &Fixture,
    lock_key: i64,
    task: &tokio::task::JoinHandle<T>,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            !task.is_finished(),
            "task completed before entering the row-lock wait; \
             the guard did not serialize it behind the in-flight approval"
        );
        // advisory lock（objid = lock_key）を待っている backend が、この barrier で
        // 止まっている承認/レビュー側。その backend にブロックされている backend を
        // 数える。
        let row = fx
            .app
            .state
            .db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*)::bigint AS count FROM pg_stat_activity w \
                 WHERE EXISTS ( \
                   SELECT 1 FROM pg_locks h \
                   WHERE h.locktype = 'advisory' AND NOT h.granted \
                     AND h.classid = 0 AND h.objid = $1 AND h.objsubid = 1 \
                     AND h.pid = ANY(pg_blocking_pids(w.pid)) )",
                [lock_key.into()],
            ))
            .await
            .expect("query row-lock waiters")
            .expect("count row");
        let count: i64 = row.try_get("", "count").expect("waiter count");
        if count > 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for a session blocked by the barrier holder"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// comparison の却下が先に更新へ入った場合、承認はその却下を読み直して止まる。
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_comparison_rejection_cannot_be_promoted_to_the_baseline() {
    let fx = setup().await;
    let build = fx.run_build("sha-race-review", &[("home", RED)]).await;
    let build_id = build_id_of(&build);
    fx.review(build_id, "home", "approve").await;

    let comparison = fx
        .comparisons(build_id)
        .await
        .into_iter()
        .find(|comparison| comparison["name"] == "home")
        .expect("home comparison");
    let comparison_id = comparison["id"].as_str().expect("comparison id");
    let condition = format!(
        "NEW.review_status::text = 'rejected' AND EXISTS (\
         SELECT 1 FROM builds WHERE id = NEW.build_id AND project_id = '{}')",
        fx.project_id
    );
    let (barrier, lock_key) = install_update_barrier(&fx, "comparisons", &condition).await;

    let review_client = fx.app.client().clone();
    let review_url = format!(
        "{}/v1/comparisons/{comparison_id}/review",
        fx.app.base_url()
    );
    let review_task = tokio::spawn(async move {
        review_client
            .post(review_url)
            .json(&json!({ "action": "reject" }))
            .send()
            .await
            .expect("reject comparison")
    });
    wait_for_advisory_waiter(&fx, lock_key).await;

    let approve_client = fx.app.client().clone();
    let approve_url = format!("{}/v1/builds/{build_id}/approve", fx.app.base_url());
    let approve_task = tokio::spawn(async move {
        approve_client
            .post(approve_url)
            .json(&json!({}))
            .send()
            .await
            .expect("approve build")
    });
    // 承認は却下側が握った build 行ロック待ちに入るはず。sleep ではなく
    // pg_blocking_pids で「barrier 保持側にブロックされた」ことを観測する。
    wait_for_row_lock_waiter(&fx, lock_key, &approve_task).await;

    barrier.commit().await.expect("release barrier");
    let reviewed = review_task.await.expect("review task");
    assert_eq!(reviewed.status(), StatusCode::OK);
    let approved = approve_task.await.expect("approve task");
    assert_eq!(approved.status(), StatusCode::CONFLICT);
    assert!(fx.current_baseline().await.is_none());
}

// build 却下が先に更新へ入った場合も、承認は却下後の build を読み直して止まる。
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_build_rejection_prevents_baseline_creation() {
    let fx = setup().await;
    let build = fx.run_build("sha-race-build", &[("home", RED)]).await;
    let build_id = build_id_of(&build);
    fx.review(build_id, "home", "approve").await;

    let condition = format!(
        "NEW.project_id = '{}' AND NEW.status::text = 'rejected'",
        fx.project_id
    );
    let (barrier, lock_key) = install_update_barrier(&fx, "builds", &condition).await;

    let reject_client = fx.app.client().clone();
    let reject_url = format!("{}/v1/builds/{build_id}/reject", fx.app.base_url());
    let reject_task = tokio::spawn(async move {
        reject_client
            .post(reject_url)
            .send()
            .await
            .expect("reject build")
    });
    wait_for_advisory_waiter(&fx, lock_key).await;

    let approve_client = fx.app.client().clone();
    let approve_url = format!("{}/v1/builds/{build_id}/approve", fx.app.base_url());
    let approve_task = tokio::spawn(async move {
        approve_client
            .post(approve_url)
            .json(&json!({}))
            .send()
            .await
            .expect("approve build")
    });
    // 承認は却下側が握った build 行ロック待ちに入るはず。
    wait_for_row_lock_waiter(&fx, lock_key, &approve_task).await;

    barrier.commit().await.expect("release barrier");
    let rejected = reject_task.await.expect("reject task");
    assert_eq!(rejected.status(), StatusCode::OK);
    let approved = approve_task.await.expect("approve task");
    assert_eq!(approved.status(), StatusCode::CONFLICT);
    assert!(fx.current_baseline().await.is_none());
}

// 他ブランチの fallback baseline は、ビルド番号だけで巻き戻り扱いしない。
#[tokio::test(flavor = "multi_thread")]
async fn older_feature_build_can_use_a_newer_main_fallback_baseline() {
    let fx = setup().await;

    let first = fx.run_build("sha-main-1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first main build");

    // feature の番号を先に確定させ、main baseline を後発ビルドで前進させる。
    let feature_id = fx
        .create_build("feature/older-build", "sha-feature", &[("home", BLUE)])
        .await;
    let newer_main = fx.run_build("sha-main-2", &[("home", BLUE)]).await;
    let res = fx
        .approve(build_id_of(&newer_main), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve newer main build");

    // feature に固有 baseline は無いため、新しい main baseline を fallback で使う。
    let status = fx
        .app
        .post_with_bearer(&format!("/v1/ci/builds/{feature_id}/finalize"), &fx.token)
        .await
        .status();
    assert_eq!(status, StatusCode::OK, "finalize older feature build");
    let feature = fx.wait_for_terminal(feature_id).await;
    assert_eq!(feature["status"], "passed");

    let res = fx.approve(feature_id, json!({ "force": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "他ブランチの fallback baseline を番号だけで巻き戻り扱いしない"
    );
}

// 却下された比較を承認できず、baseline が更新されないことを検証する。

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

// 古いビルドを後から承認できず、baseline が巻き戻らないことを検証する。

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
        message.contains("accept_revert") && message.contains("re-run"),
        "巻き戻しの明示経路と安全な代替を示すこと: {message}"
    );

    // baseline は新しい方のまま。
    let (still, _) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(still, after_third, "baseline は巻き戻っていない");

    // 専用フラグで意図を明示した場合だけ古いビルドへ戻せる。
    let res = fx
        .approve(second_id, json!({ "force": true, "accept_revert": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "明示した revert は承認できる");
    let reverted: Value = res.json().await.expect("reverted build json");
    assert_eq!(
        reverted["approval_evidence"][0]["reverted_from_build"],
        third["number"]
    );
    assert_eq!(
        reverted["approval_evidence"][0]["reverted_to_build"],
        second["number"]
    );
    let (after_revert, _) = fx.current_baseline().await.expect("baseline exists");
    assert_ne!(
        after_revert, after_third,
        "revert は新しい baseline record を作る"
    );
}

// 新しい baseline で story が増えても、明示した revert なら古い集合へ戻せる。
#[tokio::test(flavor = "multi_thread")]
async fn explicit_revert_records_stories_added_after_the_target_build() {
    let fx = setup().await;

    let old = fx.run_build("sha-old", &[("home", RED)]).await;
    let old_id = build_id_of(&old);
    let res = fx.approve(old_id, json!({ "force": true })).await;
    assert_eq!(res.status(), StatusCode::OK, "approve old baseline");

    let new = fx
        .run_build("sha-new", &[("home", BLUE), ("new-story", RED)])
        .await;
    let res = fx
        .approve(build_id_of(&new), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve expanded baseline");

    let res = fx.approve(old_id, json!({ "accept_revert": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "an explicit revert may remove stories that only exist in the newer baseline"
    );
    let reverted: Value = res.json().await.expect("reverted build json");
    assert_eq!(
        reverted["approval_evidence"][0]["removed_by_revert"],
        json!(["new-story"])
    );
    let (_, names) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(names, vec!["home".to_string()]);
}

// 通常承認そのものは証跡を増やさない。一方、その承認済み build を明示的に
// 巻き戻し再承認する場合は、上書きされる承認者と承認時刻を復元可能にする。
#[tokio::test(flavor = "multi_thread")]
async fn explicit_revert_preserves_the_superseded_normal_approval() {
    let fx = setup().await;

    let old = fx.run_build("sha-superseded-old", &[("home", RED)]).await;
    let old_id = build_id_of(&old);
    let res = fx.approve(old_id, json!({ "force": true })).await;
    assert_eq!(res.status(), StatusCode::OK, "approve old baseline");
    let originally_approved: Value = res.json().await.expect("original approval json");
    assert!(
        originally_approved["approval_evidence"].is_null(),
        "a normal approval must not create evidence"
    );

    let new = fx.run_build("sha-superseded-new", &[("home", BLUE)]).await;
    let res = fx
        .approve(build_id_of(&new), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve newer baseline");

    let res = fx.approve(old_id, json!({ "accept_revert": true })).await;
    assert_eq!(res.status(), StatusCode::OK, "re-approve old baseline");
    let reverted: Value = res.json().await.expect("reverted build json");
    let evidence = reverted["approval_evidence"]
        .as_array()
        .expect("approval evidence array");
    assert_eq!(evidence.len(), 1, "revert creates the first evidence entry");
    assert_eq!(
        evidence[0]["superseded_approved_by"], originally_approved["approved_by"],
        "the overwritten approver remains recoverable"
    );
    assert_eq!(
        evidence[0]["superseded_approved_at"], originally_approved["approved_at"],
        "the overwritten approval time remains recoverable"
    );

    let newer = fx
        .run_build("sha-superseded-newer", &[("home", BLUE)])
        .await;
    let res = fx
        .approve(build_id_of(&newer), json!({ "force": true }))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "approve another newer baseline"
    );

    let res = fx.approve(old_id, json!({ "accept_revert": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "re-approve old baseline again"
    );
    let reverted_again: Value = res.json().await.expect("second reverted build json");
    let evidence = reverted_again["approval_evidence"]
        .as_array()
        .expect("approval evidence array");
    assert_eq!(evidence.len(), 2, "each revert appends one evidence entry");
    assert_eq!(
        evidence[0]["superseded_approved_at"], originally_approved["approved_at"],
        "the first approval remains recoverable after repeated reverts"
    );
    assert_eq!(
        evidence[1]["superseded_approved_by"], reverted["approved_by"],
        "the next overwritten approver is appended"
    );
    assert_eq!(
        evidence[1]["superseded_approved_at"], reverted["approved_at"],
        "the next overwritten approval time is appended"
    );
}

// accept_revert は却下済み比較を上書きしない。
#[tokio::test(flavor = "multi_thread")]
async fn explicit_revert_does_not_override_a_rejected_comparison() {
    let fx = setup().await;

    let first = fx.run_build("sha-reject-1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let older = fx.run_build("sha-reject-2", &[("home", BLUE)]).await;
    let newer = fx.run_build("sha-reject-3", &[("home", BLUE)]).await;
    let older_id = build_id_of(&older);
    fx.review(older_id, "home", "reject").await;
    let res = fx
        .approve(build_id_of(&newer), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = fx
        .approve(older_id, json!({ "force": true, "accept_revert": true }))
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(error_message(res).await.contains("rejected"));
}

// フラグだけで通常の baseline 移動を revert 経路へ混ぜない。
#[tokio::test(flavor = "multi_thread")]
async fn accept_revert_is_rejected_when_the_approval_is_not_a_revert() {
    let fx = setup().await;

    let first = fx.run_build("sha-not-revert-1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let second = fx.run_build("sha-not-revert-2", &[("home", BLUE)]).await;
    let res = fx
        .approve(
            build_id_of(&second),
            json!({ "force": true, "accept_revert": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(error_message(res).await.contains("not a revert"));
}

// retention 後に source build が消えた baseline では、世代を推測せず状況を示す。
#[tokio::test(flavor = "multi_thread")]
async fn accept_revert_reports_when_the_baseline_source_was_not_retained() {
    let fx = setup().await;

    let first = fx.run_build("sha-retained-1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let older = fx.run_build("sha-retained-2", &[("home", BLUE)]).await;
    let newer = fx.run_build("sha-retained-3", &[("home", BLUE)]).await;
    let res = fx
        .approve(build_id_of(&newer), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let (baseline_id, _) = fx.current_baseline().await.expect("baseline exists");
    let baseline = entity::baselines::Entity::find_by_id(baseline_id)
        .one(&fx.app.state.db)
        .await
        .expect("load baseline")
        .expect("baseline row");
    let mut active: entity::baselines::ActiveModel = baseline.into();
    active.source_build_id = Set(None);
    active
        .update(&fx.app.state.db)
        .await
        .expect("model retention removing the source build reference");

    let res = fx
        .approve(
            build_id_of(&older),
            json!({ "force": true, "accept_revert": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let message = error_message(res).await;
    assert!(message.contains("no longer retained"), "{message}");
    assert!(message.contains("cannot be verified"), "{message}");
}

// force でも story の削除を明示確認なしに承認しないことを検証する。

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
    let approved: Value = res.json().await.expect("approved build json");
    assert_eq!(
        approved["approval_evidence"][0]["accepted_removals"],
        json!(["legacy"]),
        "消えた story 名を build record に残す"
    );
    let (_, names) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(names, vec!["home".to_string()]);
}

// 承認後段の manifest ガードで止まった場合、一括レビュー更新も rollback される。
#[tokio::test(flavor = "multi_thread")]
async fn manifest_guard_rolls_back_bulk_review_updates() {
    let fx = setup().await;

    let first = fx
        .run_build("sha-rollback-1", &[("home", RED), ("legacy", RED)])
        .await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let second = fx.run_build("sha-rollback-2", &[("home", BLUE)]).await;
    let second_id = build_id_of(&second);
    entity::comparisons::Entity::delete_many()
        .filter(entity::comparisons::Column::BuildId.eq(second_id))
        .filter(entity::comparisons::Column::Name.eq("legacy"))
        .exec(&fx.app.state.db)
        .await
        .expect("remove comparison to model an unexplained capture gap");

    let res = fx.approve(second_id, json!({ "force": true })).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(error_message(res).await.contains("legacy"));

    let home = entity::comparisons::Entity::find()
        .filter(entity::comparisons::Column::BuildId.eq(second_id))
        .filter(entity::comparisons::Column::Name.eq("home"))
        .one(&fx.app.state.db)
        .await
        .expect("load home comparison")
        .expect("home comparison exists");
    assert_eq!(
        home.review_status,
        entity::comparisons::ReviewStatus::Pending,
        "bulk approval before the manifest guard must be rolled back"
    );
    assert!(fx.current_baseline().await.is_some());
    assert_eq!(fx.build_status(second_id).await, "changes_detected");
}

// 個別レビューで消滅を承認した場合は、force 用フラグなしでも manifest ガードを通る。
#[tokio::test(flavor = "multi_thread")]
async fn individually_approved_removal_shrinks_the_baseline_without_bulk_flag() {
    let fx = setup().await;

    let first = fx
        .run_build("sha1", &[("home", RED), ("legacy", RED)])
        .await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");

    let second = fx.run_build("sha2", &[("home", RED)]).await;
    let second_id = build_id_of(&second);
    assert_eq!(second["removed_count"], 1);
    fx.review(second_id, "legacy", "approve").await;

    let res = fx.approve(second_id, json!({})).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "an individually reviewed removal needs no bulk-approval flag"
    );
    let (_, names) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(names, vec!["home".to_string()]);
}

// 比較失敗は force だけで承認すると、未検証の画像が baseline に焼き付く。
#[tokio::test(flavor = "multi_thread")]
async fn force_requires_explicit_acknowledgement_for_failed_comparisons() {
    let fx = setup().await;

    let first = fx.run_build("sha1", &[("home", RED)]).await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve first build");
    let (baseline_id, _) = fx.current_baseline().await.expect("baseline exists");

    let second = fx.run_build("sha2", &[("home", BLUE)]).await;
    let second_id = build_id_of(&second);
    let comparison = entity::comparisons::Entity::find()
        .filter(entity::comparisons::Column::BuildId.eq(second_id))
        .one(&fx.app.state.db)
        .await
        .expect("load comparison")
        .expect("comparison exists");
    let mut active: entity::comparisons::ActiveModel = comparison.into();
    active.status = Set(entity::comparisons::ComparisonStatus::Failed);
    active
        .update(&fx.app.state.db)
        .await
        .expect("mark comparison failed");

    let res = fx.approve(second_id, json!({ "force": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "force alone must not approve a failed comparison"
    );
    let message = error_message(res).await;
    assert!(
        message.contains("accept_failures"),
        "actionable error: {message}"
    );
    let (still, _) = fx.current_baseline().await.expect("baseline exists");
    assert_eq!(still, baseline_id, "failed image was not promoted");

    let res = fx
        .approve(second_id, json!({ "force": true, "accept_failures": true }))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "a separate explicit acknowledgement permits the exceptional operation"
    );
    let approved: Value = res.json().await.expect("approved build json");
    assert_eq!(
        approved["approval_evidence"][0]["accepted_failures"],
        json!(["home"]),
        "failed screenshot name is retained as approval evidence"
    );
}

// 通常のレビューと承認では baseline が更新されることを検証する。

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

/// content_hash migration 前に upload 済みだった build は screenshots.content_hash が
/// NULL のままでも、承認時の実体 hash + full decode を正準値として昇格できる。
#[tokio::test(flavor = "multi_thread")]
async fn existing_build_with_pre_migration_screenshot_can_be_approved_directly() {
    let fx = setup().await;
    let build = fx.run_build("legacy-shot", &[("home", RED)]).await;
    let build_id = build_id_of(&build);

    let shot = entity::screenshots::Entity::find()
        .filter(entity::screenshots::Column::BuildId.eq(build_id))
        .one(&fx.app.state.db)
        .await
        .expect("query screenshot")
        .expect("screenshot exists");
    let actual_hash = shot.content_hash.clone().expect("new upload has hash");
    let mut active: entity::screenshots::ActiveModel = shot.into();
    active.content_hash = Set(None);
    active
        .update(&fx.app.state.db)
        .await
        .expect("emulate pre-migration screenshot row");

    assert!(
        !service::screenshots::content_hashes_match(Some(&actual_hash), None),
        "positive control: the pre-fix strict expected-hash check rejected this row"
    );
    let res = fx.approve(build_id, json!({ "force": true })).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "legacy build approves directly"
    );

    let (baseline_id, _) = fx.current_baseline().await.expect("baseline exists");
    let entry = service::baselines::entries(&fx.app.state.db, baseline_id)
        .await
        .expect("baseline entries")
        .into_iter()
        .find(|entry| entry.name == "home")
        .expect("home baseline entry");
    assert_eq!(entry.content_hash.as_deref(), Some(actual_hash.as_str()));
    assert_eq!(entry.verified_content_hash, entry.content_hash);
}

// capture plan の添付は「添付時点の最新 baseline」を固定する契約を持つ。
// 添付経路が build 行しかロックしないと、baseline を検証してから固定するまでの
// 間に、別 build の承認（project 行をロックして新 baseline を作る）が割り込んで
// baseline を進められる。すると添付は 409 を返さず古い baseline を固定してしまう。
// 添付経路も承認経路と同じ build -> project の順で project 行をロックすることで、
// この競合が直列化されることを検証する。
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_approval_blocks_stale_capture_plan_pin() {
    let fx = setup().await;

    // 起点 baseline B0（source = sha-a、エントリ home/about）。
    let first = fx
        .run_build("sha-a", &[("home", RED), ("about", RED)])
        .await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve baseline build");

    // baseline を進める承認対象 D（home が変わっている）。事前にレビューを済ませ、
    // 承認そのものだけをレースにかける。
    let mover = fx
        .run_build("sha-d", &[("home", BLUE), ("about", RED)])
        .await;
    let mover_id = build_id_of(&mover);
    assert_eq!(mover["status"], "changes_detected");
    fx.review(mover_id, "home", "approve").await;

    // capture plan を添付する対象 C。撮影前・アップロード前の pending。
    let plan_build = fx.create_build("main", "sha-c", &[]).await;

    // 承認の UPDATE builds（status = approved）を trigger で止め、承認 txn に
    // project 行ロックを保持させたまま添付を競合させる。
    let condition = format!(
        "NEW.status::text = 'approved' AND NEW.project_id = '{}'",
        fx.project_id
    );
    let (barrier, lock_key) = install_update_barrier(&fx, "builds", &condition).await;

    let approve_client = fx.app.client().clone();
    let approve_url = format!("{}/v1/builds/{mover_id}/approve", fx.app.base_url());
    let approve_task = tokio::spawn(async move {
        approve_client
            .post(approve_url)
            .json(&json!({}))
            .send()
            .await
            .expect("approve mover build")
    });
    // 承認が UPDATE builds に到達し、project 行ロックを握ったまま barrier で待つ。
    wait_for_advisory_waiter(&fx, lock_key).await;

    // 添付は承認と同じ project 行ロックを待つはずなので、この時点では完了しない。
    let attach_client = fx.app.client().clone();
    let attach_url = format!("{}/v1/ci/builds/{plan_build}/plan", fx.app.base_url());
    let attach_token = fx.token.clone();
    let attach_task = tokio::spawn(async move {
        attach_client
            .post(attach_url)
            .bearer_auth(attach_token)
            .json(&json!({
                "selected_names": ["home"],
                "manifest_names": ["about", "home"],
                "baseline_commit_sha": "sha-a",
            }))
            .send()
            .await
            .expect("attach capture plan")
    });
    // 添付は承認が握った project 行ロック待ちに入るはず。sleep ではなく
    // pg_blocking_pids で「承認にブロックされた」ことを観測してから解放する。
    wait_for_row_lock_waiter(&fx, lock_key, &attach_task).await;

    // barrier を解放して承認をコミットさせる（baseline が B0 → B1(sha-d) に進む）。
    barrier.commit().await.expect("release barrier");
    let approved = approve_task.await.expect("approve task");
    assert_eq!(approved.status(), StatusCode::OK, "approval succeeds");

    // 添付は承認後に project ロックを取り、進んだ baseline を読むので、
    // 計画の起点（sha-a）とずれて 409 になる。古い baseline を黙って固定しない。
    let attached = attach_task.await.expect("attach task");
    assert_eq!(
        attached.status(),
        StatusCode::CONFLICT,
        "attachment must reject once the baseline moved instead of pinning the stale one"
    );
    let message = error_message(attached).await;
    assert!(
        message.contains("baseline moved"),
        "unexpected conflict message: {message}"
    );

    // 計画は固定されていない（baseline_id が残っていない）。
    let build = service::builds::get_build(&fx.app.state.db, plan_build)
        .await
        .expect("load plan build");
    assert!(
        build.baseline_id.is_none(),
        "no baseline must be pinned when the attachment is rejected"
    );
}

// storybook の部分レンダリング（only_story_ids）は finalize 時に「起点 baseline を
// 照合してから baseline_id に固定する」契約を持つ。attach_capture_plan と同じく、
// finalize が build 行しかロックしないと照合〜固定の間に別 build の承認（project 行を
// ロックして新 baseline を作る）が割り込み、進んだ baseline を黙って固定してしまう。
// finalize_storybook も承認と同じ build -> project の順で project 行をロックすることで
// この競合が直列化され、baseline が動いていれば 409 で弾かれることを検証する。
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_approval_blocks_stale_storybook_finalize_pin() {
    let fx = setup().await;

    // 起点 baseline B0（source = sha-a、エントリ home/about）。
    let first = fx
        .run_build("sha-a", &[("home", RED), ("about", RED)])
        .await;
    let res = fx
        .approve(build_id_of(&first), json!({ "force": true }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve baseline build");

    // baseline を進める承認対象 D（home が変わっている）。承認そのものだけをレースにかける。
    let mover = fx
        .run_build("sha-d", &[("home", BLUE), ("about", RED)])
        .await;
    let mover_id = build_id_of(&mover);
    assert_eq!(mover["status"], "changes_detected");
    fx.review(mover_id, "home", "approve").await;

    // 部分レンダリング対象の storybook ビルド C。HTTP 作成は Chromium 設定ゲートに
    // 掛かるので、サービス層で pending の storybook ビルドを直接用意する（finalize
    // エンドポイント自体はゲートしない）。bundle 内容は検証されないのでキーだけ付ける。
    let project_model = entity::projects::Entity::find_by_id(fx.project_id)
        .one(&fx.app.state.db)
        .await
        .expect("load project")
        .expect("project exists");
    let sb_build = service::builds::create_build(
        &fx.app.state.db,
        &project_model,
        "main".to_string(),
        "sha-c".to_string(),
        None,
        None,
        entity::builds::BuildMode::Storybook,
    )
    .await
    .expect("create storybook build");
    let sb_build = service::builds::attach_storybook_bundle(
        &fx.app.state.db,
        sb_build,
        "test-key".to_string(),
    )
    .await
    .expect("attach storybook bundle");
    let sb_id = sb_build.id;

    // 承認の UPDATE builds（status = approved）を trigger で止め、承認 txn に
    // project 行ロックを保持させたまま finalize を競合させる。
    let condition = format!(
        "NEW.status::text = 'approved' AND NEW.project_id = '{}'",
        fx.project_id
    );
    let (barrier, lock_key) = install_update_barrier(&fx, "builds", &condition).await;

    let approve_client = fx.app.client().clone();
    let approve_url = format!("{}/v1/builds/{mover_id}/approve", fx.app.base_url());
    let approve_task = tokio::spawn(async move {
        approve_client
            .post(approve_url)
            .json(&json!({}))
            .send()
            .await
            .expect("approve mover build")
    });
    // 承認が UPDATE builds に到達し、project 行ロックを握ったまま barrier で待つ。
    wait_for_advisory_waiter(&fx, lock_key).await;

    // finalize は承認と同じ project 行ロック待ちに入るはず。
    let finalize_client = fx.app.client().clone();
    let finalize_url = format!("{}/v1/ci/builds/{sb_id}/finalize", fx.app.base_url());
    let finalize_token = fx.token.clone();
    let finalize_task = tokio::spawn(async move {
        finalize_client
            .post(finalize_url)
            .bearer_auth(finalize_token)
            .json(&json!({
                "only_story_ids": ["home--default"],
                "expected_baseline_commit_sha": "sha-a",
            }))
            .send()
            .await
            .expect("finalize storybook build")
    });
    // sleep ではなく pg_blocking_pids で「承認にブロックされた」ことを観測してから解放。
    wait_for_row_lock_waiter(&fx, lock_key, &finalize_task).await;

    // barrier を解放して承認をコミットさせる（baseline が B0 → B1(sha-d) に進む）。
    barrier.commit().await.expect("release barrier");
    let approved = approve_task.await.expect("approve task");
    assert_eq!(approved.status(), StatusCode::OK, "approval succeeds");

    // finalize は承認後に project ロックを取り、進んだ baseline を読むので、
    // 計画の起点（sha-a）とずれて 409 になる。古い baseline を黙って固定しない。
    let finalized = finalize_task.await.expect("finalize task");
    assert_eq!(
        finalized.status(),
        StatusCode::CONFLICT,
        "finalize must reject once the baseline moved instead of pinning the stale one"
    );
    let message = error_message(finalized).await;
    assert!(
        message.contains("does not match the current baseline"),
        "unexpected conflict message: {message}"
    );

    // baseline_id は固定されず、状態も pending のまま（rendering へ遷移していない）。
    let build = service::builds::get_build(&fx.app.state.db, sb_id)
        .await
        .expect("load storybook build");
    assert!(
        build.baseline_id.is_none(),
        "no baseline must be pinned when the finalize is rejected"
    );
    assert_eq!(
        build.status,
        entity::builds::BuildStatus::Pending,
        "rejected finalize must leave the build pending"
    );
}
