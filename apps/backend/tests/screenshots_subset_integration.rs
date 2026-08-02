//! screenshots モードの部分アップロード（capture plan / carry-forward）の統合テスト。
//!
//! 検証する契約:
//!
//! 1. 撮影前に `POST /v1/ci/builds/{id}/plan` で固定した計画の選択外 baseline
//!    エントリは `removed` ではなく前回 baseline の流用（unchanged）になり、
//!    承認しても baseline から消えない
//! 2. 計画の manifest（現行 index）から消えた名前は流用されず `removed` になる
//!    （story の削除が carry-forward で隠れない）
//! 3. 「今回撮る集合」の出所は保存済み計画であり、finalize の自己申告ではない。
//!    計画なしのビルドへの `captured_names` は拒否され、計画ありのビルドは
//!    アップロード実績が計画と一致しない限り finalize できない
//!    （撮影が全滅して空アップロードになっても偽 PASS しない）
//! 4. 比較 baseline は計画添付時に照合のうえ固定され、その後最新 baseline が
//!    動いても比較対象はずれない。計画の起点が古ければ添付自体が 409

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

/// 全撮影ビルドで使う 3 ページの現行 manifest。
const FULL_MANIFEST: [&str; 3] = ["about", "home", "pricing"];

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

    async fn attach_plan(
        &self,
        build_id: Uuid,
        selected: &[&str],
        manifest: &[&str],
        baseline_commit_sha: &str,
    ) -> reqwest::Response {
        self.app
            .post_json_with_bearer(
                &format!("/v1/ci/builds/{build_id}/plan"),
                &self.token,
                json!({
                    "selected_names": selected,
                    "manifest_names": manifest,
                    "baseline_commit_sha": baseline_commit_sha,
                }),
            )
            .await
    }

    async fn attach_plan_ok(
        &self,
        build_id: Uuid,
        selected: &[&str],
        manifest: &[&str],
        baseline_commit_sha: &str,
    ) {
        let res = self
            .attach_plan(build_id, selected, manifest, baseline_commit_sha)
            .await;
        assert_eq!(res.status(), StatusCode::OK, "attach capture plan");
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

/// 部分アップロード: 計画の選択外（かつ manifest 内）の baseline エントリは
/// removed にならず流用され、承認後の baseline からも消えない。
///
/// positive control: carry-forward の無い実装では about / pricing が `removed`
/// になり（removed_count = 2）、承認で baseline が 1 件に縮む → このテストは落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn subset_upload_carries_forward_unselected_baseline_entries() {
    let fx = setup().await;
    let home_v1 = png(40, 30, [255, 255, 255, 255]);
    fx.establish_baseline("base0001", home_v1).await;

    // home だけ撮り直す計画を撮影前に固定してから、home をアップロードする。
    let build = fx.create_build("subset01").await;
    let build_id = build_id_of(&build);
    assert_eq!(
        build["baseline_commit_sha"].as_str(),
        Some("base0001"),
        "creation reports the current baseline as the planning basis"
    );
    fx.attach_plan_ok(build_id, &["home"], &FULL_MANIFEST, "base0001")
        .await;
    fx.upload(build_id, "home", png(40, 30, [255, 0, 0, 255]))
        .await;
    let res = fx.finalize(build_id, None).await;
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

    // carry-forward 複製は (build_id, name) から導出した決定的 ID を使う。
    // ジョブのリトライ・再実行が同じストレージキーへ収束し、upload と insert の
    // 間で落ちた孤児オブジェクトが再実行で回収される前提を固定する。
    {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        for name in ["about", "pricing"] {
            let row = entity::screenshots::Entity::find()
                .filter(entity::screenshots::Column::BuildId.eq(build_id))
                .filter(entity::screenshots::Column::Name.eq(name))
                .one(&fx.app.state.db)
                .await
                .expect("query carried shot")
                .expect("carried shot exists");
            assert_eq!(
                row.id,
                service::screenshots::carry_forward_screenshot_id(build_id, name),
                "carried-forward `{name}` must use the deterministic id so retries converge"
            );
        }
    }

    // 承認しても選択外のエントリは baseline に残る（消滅しない）。
    fx.approve_force(build_id).await;
    let (_, names) = fx.latest_baseline().await;
    assert_eq!(
        names,
        vec!["about", "home", "pricing"],
        "the promoted baseline keeps carried-forward entries"
    );
}

/// 削除された story は carry-forward で隠れない: 計画の manifest から消えた
/// baseline エントリは流用されず `removed` として報告される。
///
/// positive control: manifest を見ず選択外を無差別に流用する実装では pricing が
/// unchanged に化けて removed_count = 0 になり、このテストは落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn vanished_story_is_reported_as_removed_not_carried_forward() {
    let fx = setup().await;
    fx.establish_baseline("base0006", png(40, 30, [255, 255, 255, 255]))
        .await;

    // 現行 index から pricing が消えた状態で home だけ撮り直す計画。
    let build = fx.create_build("vanish01").await;
    let build_id = build_id_of(&build);
    fx.attach_plan_ok(build_id, &["home"], &["about", "home"], "base0006")
        .await;
    fx.upload(build_id, "home", png(40, 30, [255, 255, 255, 255]))
        .await;
    assert_eq!(fx.finalize(build_id, None).await.status(), StatusCode::OK);

    let build = fx.wait_for_terminal(build_id).await;
    assert_eq!(build["status"].as_str(), Some("changes_detected"));
    assert_eq!(build["total_count"].as_i64(), Some(3));
    assert_eq!(
        build["removed_count"].as_i64(),
        Some(1),
        "a story missing from the manifest must surface as removed"
    );
    assert_eq!(
        build["unchanged_count"].as_i64(),
        Some(2),
        "home (re-captured, identical) and about (carried forward)"
    );

    let cmps = fx.comparisons(build_id).await;
    assert_eq!(
        find_comparison(&cmps, "pricing")["status"],
        "removed",
        "the vanished story is visible for review instead of being silently reused"
    );
    assert_eq!(find_comparison(&cmps, "about")["status"], "unchanged");
}

/// 何も撮らない計画（selected が空）は全エントリ流用で passed になる。
/// 撮影前に固定された計画が「変更なし」を宣言しているので、これは偽 PASS ではない。
#[tokio::test(flavor = "multi_thread")]
async fn empty_selection_plan_reuses_the_whole_baseline() {
    let fx = setup().await;
    fx.establish_baseline("base0002", png(40, 30, [255, 255, 255, 255]))
        .await;

    let build = fx.create_build("subset02").await;
    let build_id = build_id_of(&build);
    fx.attach_plan_ok(build_id, &[], &FULL_MANIFEST, "base0002")
        .await;
    let res = fx.finalize(build_id, None).await;
    assert_eq!(res.status(), StatusCode::OK, "empty selection finalize");

    let build = fx.wait_for_terminal(build_id).await;
    assert_eq!(build["status"].as_str(), Some("passed"));
    assert_eq!(build["total_count"].as_i64(), Some(3));
    assert_eq!(build["unchanged_count"].as_i64(), Some(3));
    assert_eq!(build["removed_count"].as_i64(), Some(0));
}

/// 「撮る集合」は保存済み計画からのみ来る。
///
/// - 計画なしのビルドへの `captured_names` は 400（自己申告だけの部分アップロードは
///   撮影全滅時に「空の申告 == 空のアップロード」で偽 PASS するため受け付けない）
/// - 計画ありでもアップロードが計画に満たなければ finalize は 400
/// - 計画に無い名前のアップロードは upload 時点で 400
///
/// positive control: 申告を出所にする実装では最初の finalize
/// （宣言 [] == アップロード 0 枚）が 200 で通ってしまい、このテストは落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn selection_comes_from_the_stored_plan_not_the_callers_declaration() {
    let fx = setup().await;
    fx.establish_baseline("base0003", png(40, 30, [255, 255, 255, 255]))
        .await;

    // 計画なし + captured_names（空 = 全流用の主張）→ 400。
    let unplanned = fx.create_build("noplan01").await;
    let unplanned_id = build_id_of(&unplanned);
    let res = fx
        .finalize(unplanned_id, Some(json!({ "captured_names": [] })))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "captured_names without a pre-capture plan must be rejected"
    );
    let body = res.text().await.expect("body");
    assert!(
        body.contains("capture plan"),
        "the error should point the caller at the plan endpoint: {body}"
    );

    // 計画あり（home を撮るはず）なのに 1 枚もアップロードせず finalize → 400。
    // 撮影が全滅したケース。ここが通ると全 baseline 流用の偽 PASS になる。
    let planned = fx.create_build("plan0001").await;
    let planned_id = build_id_of(&planned);
    fx.attach_plan_ok(planned_id, &["home"], &FULL_MANIFEST, "base0003")
        .await;
    let res = fx.finalize(planned_id, None).await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a non-empty plan with zero uploads must not finalize"
    );

    // 申告で計画を上書きすることもできない（申告 [] は計画と不一致で 400）。
    let res = fx
        .finalize(planned_id, Some(json!({ "captured_names": [] })))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "an empty declaration cannot override the stored plan"
    );

    // 計画に無い名前のアップロードは upload 時点で拒否される。
    let res = fx
        .app
        .upload_screenshot(planned_id, &fx.token, "about", png(8, 8, [1, 2, 3, 255]))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "uploads outside the planned selection are rejected eagerly"
    );

    // 計画どおりアップロードすれば通る（captured_names のクロスチェックも一致）。
    fx.upload(planned_id, "home", png(40, 30, [9, 9, 9, 255]))
        .await;
    let res = fx
        .finalize(planned_id, Some(json!({ "captured_names": ["home"] })))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "plan fulfilled");
}

/// screenshots モードの only_story_ids は引き続き 400（capture plan を使う）。
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
        body.contains("capture plan"),
        "the error should point the caller at the plan endpoint: {body}"
    );
}

/// 比較 baseline は計画添付時に固定される。
///
/// 添付後に別ビルドの承認で最新 baseline が動いても、比較は固定した baseline に
/// 対して行われる（クライアントが計画に使った起点とずれない）。
///
/// positive control: 比較時に最新 baseline を引き直す実装では、このビルドは
/// 新 baseline（home v2）と比較されて changes_detected になり、このテストは落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn comparison_uses_the_baseline_pinned_when_the_plan_was_attached() {
    let fx = setup().await;
    let home_v1 = png(40, 30, [255, 255, 255, 255]);
    fx.establish_baseline("base0004", home_v1.clone()).await;
    let (pinned_baseline_id, _) = fx.latest_baseline().await;

    // 計画添付でこのビルドは作成時点の baseline（home v1）に固定される。
    let pinned_build = fx.create_build("pinned01").await;
    let pinned_build_id = build_id_of(&pinned_build);
    assert_eq!(
        pinned_build["baseline_commit_sha"].as_str(),
        Some("base0004")
    );
    fx.attach_plan_ok(pinned_build_id, &["home"], &FULL_MANIFEST, "base0004")
        .await;

    // 別ビルドが home v2 で承認され、最新 baseline が入れ替わる。
    fx.establish_baseline("moved001", png(40, 30, [0, 0, 255, 255]))
        .await;
    let (latest_baseline_id, _) = fx.latest_baseline().await;
    assert_ne!(pinned_baseline_id, latest_baseline_id, "baseline moved");

    // v1 と同一の home をアップロード → 固定 baseline と比較して passed。
    fx.upload(pinned_build_id, "home", home_v1).await;
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

/// 計画の起点が古い（添付前に baseline が動いた）場合、添付は 409 で拒否される。
/// 固定なしビルドへの expected_baseline_commit_sha も 400 で拒否される。
#[tokio::test(flavor = "multi_thread")]
async fn stale_plan_basis_is_rejected() {
    let fx = setup().await;
    fx.establish_baseline("base0005", png(40, 30, [255, 255, 255, 255]))
        .await;

    // ビルド作成の後、添付の前に baseline が動く。
    let build = fx.create_build("stale001").await;
    let build_id = build_id_of(&build);
    fx.establish_baseline("moved002", png(40, 30, [0, 255, 0, 255]))
        .await;

    // 旧 baseline を起点にした計画の添付 → 409（再計画が必要）。
    let res = fx
        .attach_plan(build_id, &["home"], &FULL_MANIFEST, "base0005")
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT, "stale plan basis");
    let body = res.text().await.expect("body");
    assert!(
        body.contains("baseline moved"),
        "error should say the baseline moved: {body}"
    );

    // 計画（= 固定 baseline）なしのビルドに expected を渡しても照合対象が無い → 400。
    fx.upload(build_id, "home", png(8, 8, [1, 2, 3, 255])).await;
    let res = fx
        .finalize(
            build_id,
            Some(json!({ "expected_baseline_commit_sha": "base0005" })),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.text().await.expect("body");
    assert!(
        body.contains("capture plan"),
        "error should explain that verification needs a plan: {body}"
    );
}

/// plan 添付と upload は build 行ロック（review_lock::build）で直列化される。
///
/// 添付処理が build 行をロックしている間に届いたアップロードは、添付の commit
/// までブロックされ、commit 後に保存済み計画で検証される。ここでは「計画外の
/// 名前」を送るので 400 になり、DB 行も残らない（ストレージは補償削除）。
///
/// positive control: ロックの無い実装（build 行ロックを取らずに挿入する旧経路）
/// では、アップロードは添付中でも待たされず即 201 で通り、
/// `!handle.is_finished()` と最終ステータス 400 の両方が落ちる。
/// これは「計画添付前の検査をすり抜けた計画外ショット」そのものであり、
/// アップロード実績から計画を逆算する偽 PASS 経路（添付時の
/// 「アップロード済みなら 409」検査の素通り）と同型である。
#[tokio::test(flavor = "multi_thread")]
async fn uploads_are_serialized_with_plan_attachment_on_the_build_row() {
    use sea_orm::{
        ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
        TransactionTrait,
    };

    let fx = setup().await;
    fx.establish_baseline("base0008", png(40, 30, [255, 255, 255, 255]))
        .await;
    let build = fx.create_build("serial01").await;
    let build_id = build_id_of(&build);
    let (baseline_id, _) = fx.latest_baseline().await;

    // 添付処理の途中を再現する: build 行ロックを保持したまま止まっている
    // トランザクション（attach_capture_plan がロック直後で停止している状態）。
    let txn = fx.app.state.db.begin().await.expect("begin");
    let locked = service::review_lock::build(&txn, build_id)
        .await
        .expect("lock build row");

    // その間に届いた「計画外の名前」のアップロード（home だけ撮る計画になる）。
    let base_url = fx.app.base_url().to_string();
    let token = fx.token.clone();
    let handle = tokio::spawn(async move {
        let part = reqwest::multipart::Part::bytes(png(8, 8, [1, 2, 3, 255]))
            .file_name("pricing.png")
            .mime_str("image/png")
            .expect("png mime");
        let form = reqwest::multipart::Form::new()
            .text("name", "pricing")
            .part("file", part);
        reqwest::Client::new()
            .post(format!("{base_url}/v1/ci/builds/{build_id}/screenshots"))
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .expect("multipart upload")
            .status()
    });

    // ロックが効いていればアップロードは commit まで完了できない。
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !handle.is_finished(),
        "the upload must block on the build row lock while a plan attachment is in flight"
    );

    // ロックを保持したまま計画を添付して commit（home のみ選択）。
    let mut active: entity::builds::ActiveModel = locked.into();
    active.capture_plan = Set(Some(serde_json::json!({
        "selected_names": ["home"],
        "manifest_names": FULL_MANIFEST,
    })));
    active.baseline_id = Set(Some(baseline_id));
    active
        .update(&txn)
        .await
        .expect("attach plan under the lock");
    txn.commit().await.expect("commit");

    // commit 後、アップロードは保存済み計画で検証されて 400 で落ちる。
    let status = handle.await.expect("join upload");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an out-of-plan upload that raced the attachment must be rejected, not inserted"
    );

    // DB 行は残らない（補償が効いている）。
    let rows = entity::screenshots::Entity::find()
        .filter(entity::screenshots::Column::BuildId.eq(build_id))
        .count(&fx.app.state.db)
        .await
        .expect("count screenshots");
    assert_eq!(
        rows, 0,
        "the rejected upload must not leave a screenshot row"
    );
}

/// 計画はアップロード開始前にしか添付できない（撮影結果からの逆算を断つ）。
/// selected が manifest の部分集合でない計画・二重添付も拒否される。
#[tokio::test(flavor = "multi_thread")]
async fn plan_attachment_guards() {
    let fx = setup().await;
    fx.establish_baseline("base0007", png(40, 30, [255, 255, 255, 255]))
        .await;

    // アップロード後の添付は 409。
    let build = fx.create_build("guard001").await;
    let build_id = build_id_of(&build);
    fx.upload(build_id, "home", png(8, 8, [1, 2, 3, 255])).await;
    let res = fx
        .attach_plan(build_id, &["home"], &FULL_MANIFEST, "base0007")
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "a plan attached after uploads could be derived from what happened to be captured"
    );

    // selected ⊄ manifest は 400。
    let build = fx.create_build("guard002").await;
    let build_id = build_id_of(&build);
    let res = fx
        .attach_plan(build_id, &["home", "ghost"], &FULL_MANIFEST, "base0007")
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "selected ⊄ manifest");

    // 正常添付ののち、二重添付は 409。
    fx.attach_plan_ok(build_id, &["home"], &FULL_MANIFEST, "base0007")
        .await;
    let res = fx
        .attach_plan(build_id, &["home"], &FULL_MANIFEST, "base0007")
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT, "double attach");
}

/// 名前規則の一箇所寄せ: 計画・アップロード・finalize の `captured_names` は
/// すべて同じ規則（空でない・前後空白なし・255 **バイト**以内）で検証される。
///
/// positive control: 規則が経路ごとに二重定義だった実装（計画側 512 文字上限で
/// 空白素通し、アップロード側 trim + 255 バイト）では、256 バイトの名前や
/// 空白付きの名前が**計画には載ってしまう**——アップロードだけが拒否され、
/// finalize は計画とアップロードの完全一致を要求するため、そのビルドは永久に
/// finalize できない。その実装では本テストの計画側 400 アサーションが落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn name_rules_are_identical_across_plan_upload_and_finalize() {
    let fx = setup().await;
    fx.establish_baseline("base0008", png(40, 30, [255, 255, 255, 255]))
        .await;

    // 256 バイトの名前はアップロードで通らないので、計画の時点で拒否する。
    let build = fx.create_build("names001").await;
    let build_id = build_id_of(&build);
    let long = "a".repeat(256);
    let manifest: Vec<&str> = vec!["home", "about", "pricing", long.as_str()];
    let res = fx
        .attach_plan(build_id, &[long.as_str()], &manifest, "base0008")
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a 256-byte name can never be uploaded, so the plan must reject it up front"
    );
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("bytes"),
        "the limit is bytes, not characters, and the error must say so: {body}"
    );

    // 前後空白付きの名前も同様（アップロード側は trim せず拒否するため、
    // 計画に載せると突き合わせが永久に成立しない）。
    let res = fx
        .attach_plan(
            build_id,
            &["home "],
            &["home ", "about", "pricing"],
            "base0008",
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a whitespace-padded name must be rejected at plan time"
    );
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("whitespace"),
        "the error must explain the whitespace rule: {body}"
    );

    // 境界値: ちょうど 255 バイトはどの経路でも有効。
    let max = "a".repeat(255);
    let manifest: Vec<&str> = vec!["home", "about", "pricing", max.as_str()];
    fx.attach_plan_ok(build_id, &[max.as_str()], &manifest, "base0008")
        .await;

    // アップロードは同じ規則で拒否する。かつては黙って trim して受けていたが、
    // それでは計画の `home ` と保存名 `home` がずれて突き合わせが壊れる。
    let build = fx.create_build("names002").await;
    let build_id = build_id_of(&build);
    let res = fx
        .app
        .upload_screenshot(build_id, &fx.token, "home ", png(8, 8, [1, 2, 3, 255]))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "upload must reject (not silently trim) a whitespace-padded name"
    );
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("whitespace"),
        "the upload error must state the shared whitespace rule: {body}"
    );

    // finalize の captured_names も同じ規則。規則違反は「計画との不一致」より
    // 手前の、名前規則そのものの 400 として報告される。
    let build = fx.create_build("names003").await;
    let build_id = build_id_of(&build);
    fx.attach_plan_ok(build_id, &["home"], &FULL_MANIFEST, "base0008")
        .await;
    fx.upload(build_id, "home", png(8, 8, [9, 9, 9, 255])).await;
    let res = fx
        .finalize(build_id, Some(json!({ "captured_names": ["home "] })))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("whitespace"),
        "captured_names must be validated by the shared name rule, \
         not merely fail the plan cross-check: {body}"
    );

    // 正しい名前で finalize は通る（規則の締めすぎで正常経路を壊していない）。
    let res = fx
        .finalize(build_id, Some(json!({ "captured_names": ["home"] })))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "valid names still finalize");
}

/// 命名規則ずれの検出: baseline が非空・`manifest_names` も非空なのに名前が
/// 1 件も重ならない計画は、story の全削除ではなく命名の変更（例: PNG パス由来の
/// 名前で育てた baseline に story ID の manifest を当てた）とみなして 400。
///
/// positive control: このガードが無い実装では添付が通り、baseline の全エントリが
/// removed として報告されるだけで比較は成功扱いになる——命名のずれに気づけない。
/// その実装では最初の 400 アサーションが落ちる。
#[tokio::test(flavor = "multi_thread")]
async fn plan_with_zero_baseline_overlap_is_rejected_as_naming_mismatch() {
    let fx = setup().await;
    fx.establish_baseline("base0009", png(40, 30, [255, 255, 255, 255]))
        .await;

    // baseline は home / about / pricing。パス由来の別命名 manifest は交差ゼロ。
    let build = fx.create_build("names004").await;
    let build_id = build_id_of(&build);
    let res = fx
        .attach_plan(
            build_id,
            &["mobile/home"],
            &["mobile/home", "mobile/about", "mobile/pricing"],
            "base0009",
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a plan whose manifest shares no name with the baseline would silently \
         turn every baseline entry into `removed`"
    );
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("naming-scheme mismatch"),
        "the error must point at the naming mismatch and the rebuild path: {body}"
    );

    // 1 件でも重なれば添付できる（部分的な改名は removed + added として差分に
    // 見えるので、ここで拒否するのは「全滅」だけ）。
    fx.attach_plan_ok(
        build_id,
        &["mobile/home"],
        &["mobile/home", "home"],
        "base0009",
    )
    .await;
}

/// 新規 story（manifest にあり baseline に無い）を selected から落とした計画は 400。
///
/// 新規 story には carry-forward の引き継ぎ元が無いため、選択から漏れると
/// アップロードも流用もされず比較結果のどこにも現れない——CI の選択ロジックの
/// バグが「added の報告なし・レビュー可視性ゼロの PASS」に化ける。
///
/// positive control: この検査の無い実装では最初の添付が 200 で通り、
/// 400 のアサーションで落ちる。後半は同じ新規 story を selected に含めれば
/// 通ることを確認する（正当な部分撮影を弾く検査ではない）。
#[tokio::test(flavor = "multi_thread")]
async fn a_plan_that_omits_a_new_story_from_selection_is_rejected() {
    let fx = setup().await;
    fx.establish_baseline("base0011", png(40, 30, [255, 255, 255, 255]))
        .await;

    let build = fx.create_build("newsty01").await;
    let build_id = build_id_of(&build);

    // manifest に新規 "contact" が現れたのに selected に入っていない → 400。
    let res = fx
        .attach_plan(
            build_id,
            &["home"],
            &["home", "about", "pricing", "contact"],
            "base0011",
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a new story missing from selected_names would vanish from the comparison \
         without ever being reported as added"
    );
    let body = res.text().await.expect("error body");
    assert!(
        body.contains("contact") && body.contains("carry forward"),
        "the error must name the unselected new screenshot and explain why: {body}"
    );

    // 同じ計画でも新規 story を選択していれば通る。既存 story（about / pricing）は
    // baseline にあるので、絞り込み（selected から外す）は引き続き正当。
    fx.attach_plan_ok(
        build_id,
        &["home", "contact"],
        &["home", "about", "pricing", "contact"],
        "base0011",
    )
    .await;
}

/// 正当な「story を全部削除した」ケース（manifest が空）はゼロ交差ガードに
/// 当たらず、全エントリが removed として**見える形で**報告される。
#[tokio::test(flavor = "multi_thread")]
async fn plan_with_empty_manifest_reports_full_deletion_as_removed() {
    let fx = setup().await;
    fx.establish_baseline("base0010", png(40, 30, [255, 255, 255, 255]))
        .await;

    let build = fx.create_build("names005").await;
    let build_id = build_id_of(&build);
    fx.attach_plan_ok(build_id, &[], &[], "base0010").await;
    let res = fx.finalize(build_id, None).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "an empty manifest declares every story deleted; nothing is uploaded"
    );

    let build = fx.wait_for_terminal(build_id).await;
    assert_eq!(build["status"].as_str(), Some("changes_detected"));
    assert_eq!(
        build["removed_count"].as_i64(),
        Some(3),
        "full deletion is reported as removed, never hidden by carry-forward"
    );
    assert_eq!(build["unchanged_count"].as_i64(), Some(0));
}
