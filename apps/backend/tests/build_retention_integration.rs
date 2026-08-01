//! ビルド保持数（`build_retention_limit`）に基づく自動プルーニングの統合テスト。
//!
//! `service::builds::prune_old_builds` / `prune_project_builds_best_effort` を
//! 本物の Postgres + ローカルストレージ（TestApp）に対して直接叩き、DB 行と
//! ストレージ成果物の両方が期待どおり消える／残ることを確認する。

mod common;

use std::sync::Arc;

use common::TestApp;
use entity::builds::{BuildMode, BuildStatus};
use reqwest::StatusCode;
use sea_orm::DatabaseConnection;
use serde_json::json;
use service::storage::StorageBackend;
use uuid::Uuid;

async fn create_tenant(app: &TestApp, prefix: &str) -> Uuid {
    let slug = format!("{prefix}-{}", &Uuid::new_v4().to_string()[..8]);
    let response = app
        .post_json("/v1/tenants", json!({ "name": "Acme", "slug": slug }))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: serde_json::Value = response.json().await.expect("tenant json");
    Uuid::parse_str(body["id"].as_str().expect("tenant id")).expect("uuid")
}

async fn create_project(app: &TestApp, tenant_id: Uuid, slug: &str) -> Uuid {
    let response = app
        .post_json(
            &format!("/v1/tenants/{tenant_id}/projects"),
            json!({ "name": "Web", "slug": slug }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: serde_json::Value = response.json().await.expect("project json");
    Uuid::parse_str(body["id"].as_str().expect("project id")).expect("uuid")
}

/// スクリーンショット 1 枚を持つ terminal（passed）ビルドを作る。
///
/// 戻り値は `(build_id, screenshot_id, storage_key)`。ストレージには本物の PNG が置かれる。
async fn make_passed_build(
    app: &TestApp,
    tenant_id: Uuid,
    project_id: Uuid,
    sha: &str,
) -> (Uuid, Uuid, String) {
    let db = &app.state.db;
    let storage = &app.state.storage;

    let project = service::projects::get_project(db, project_id)
        .await
        .expect("load project");
    let build = service::builds::create_build(
        db,
        &project,
        "main".to_string(),
        sha.to_string(),
        None,
        None,
        BuildMode::Screenshots,
    )
    .await
    .expect("create build");

    // 本物の 2x2 PNG を保存する（ストレージ削除まで検証するため）。
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
    let png = service::screenshots::encode_png(&image).expect("encode png");
    let shot = service::screenshots::store_screenshot(
        db,
        storage,
        tenant_id,
        project_id,
        build.id,
        "home".to_string(),
        png,
    )
    .await
    .expect("store screenshot");

    let build = service::builds::transition(db, build, BuildStatus::Processing)
        .await
        .expect("to processing");
    let build = service::builds::transition(db, build, BuildStatus::Passed)
        .await
        .expect("to passed");

    (build.id, shot.id, shot.storage_key)
}

async fn object_exists(storage: &Arc<dyn StorageBackend>, key: &str) -> bool {
    storage.get_stream(key).await.is_ok()
}

async fn set_retention(db: &DatabaseConnection, project_id: Uuid, limit: Option<i32>) {
    let project = service::projects::get_project(db, project_id)
        .await
        .expect("load project");
    service::projects::update_project(
        db,
        project,
        service::projects::ProjectSettings {
            build_retention_limit: Some(limit),
            ..Default::default()
        },
    )
    .await
    .expect("update retention");
}

#[tokio::test(flavor = "multi_thread")]
async fn prune_removes_old_terminal_builds_and_their_objects() {
    let app = TestApp::new().await;
    app.login_as_new_user().await;
    let tenant_id = create_tenant(&app, "prune-old").await;
    let project_id = create_project(&app, tenant_id, "web").await;
    let db = &app.state.db;
    let storage = &app.state.storage;

    // 番号昇順に 4 件（b1 が最古、b4 が最新）。
    let b1 = make_passed_build(&app, tenant_id, project_id, "sha1").await;
    let b2 = make_passed_build(&app, tenant_id, project_id, "sha2").await;
    let b3 = make_passed_build(&app, tenant_id, project_id, "sha3").await;
    let b4 = make_passed_build(&app, tenant_id, project_id, "sha4").await;

    set_retention(db, project_id, Some(2)).await;

    let deleted = service::builds::prune_old_builds(db, storage, project_id, 2)
        .await
        .expect("prune");
    assert_eq!(deleted, 2, "2 件超過しているので 2 件消える");

    // 最新 2 件（b3, b4）が残り、古い 2 件（b1, b2）が消える。
    assert!(service::builds::get_build(db, b1.0).await.is_err());
    assert!(service::builds::get_build(db, b2.0).await.is_err());
    assert!(service::builds::get_build(db, b3.0).await.is_ok());
    assert!(service::builds::get_build(db, b4.0).await.is_ok());

    // ストレージ成果物も古い分だけ消える。
    assert!(!object_exists(storage, &b1.2).await, "b1 の PNG は消える");
    assert!(!object_exists(storage, &b2.2).await, "b2 の PNG は消える");
    assert!(object_exists(storage, &b3.2).await, "b3 の PNG は残る");
    assert!(object_exists(storage, &b4.2).await, "b4 の PNG は残る");
}

#[tokio::test(flavor = "multi_thread")]
async fn prune_keeps_baseline_referenced_builds() {
    let app = TestApp::new().await;
    app.login_as_new_user().await;
    let tenant_id = create_tenant(&app, "prune-baseline").await;
    let project_id = create_project(&app, tenant_id, "web").await;
    let db = &app.state.db;
    let storage = &app.state.storage;

    // b1 を承認して baseline の参照元にする（b1 は Approved になる）。
    let b1 = make_passed_build(&app, tenant_id, project_id, "sha1").await;
    let reviewer = app.insert_user().await;
    let build1 = service::builds::get_build(db, b1.0).await.expect("b1");
    service::builds::approve_build(db, build1, reviewer.id, Default::default())
        .await
        .expect("approve b1");

    let b2 = make_passed_build(&app, tenant_id, project_id, "sha2").await;
    let b3 = make_passed_build(&app, tenant_id, project_id, "sha3").await;

    // 上限 1: 最新（b3）だけ残す想定だが、baseline 参照元の b1 は保護される。
    let deleted = service::builds::prune_old_builds(db, storage, project_id, 1)
        .await
        .expect("prune");
    assert_eq!(deleted, 1, "b2 のみ削除される（b1 は baseline 参照で保護）");

    assert!(
        service::builds::get_build(db, b1.0).await.is_ok(),
        "baseline 参照元の b1 は残る"
    );
    assert!(
        service::builds::get_build(db, b2.0).await.is_err(),
        "b2 は消える"
    );
    assert!(
        service::builds::get_build(db, b3.0).await.is_ok(),
        "最新 b3 は残る"
    );

    // baseline エントリが参照する b1 のストレージ成果物も残っている。
    assert!(object_exists(storage, &b1.2).await, "b1 の PNG は残る");
    assert!(!object_exists(storage, &b2.2).await, "b2 の PNG は消える");
}

#[tokio::test(flavor = "multi_thread")]
async fn prune_is_noop_when_retention_is_unlimited() {
    let app = TestApp::new().await;
    app.login_as_new_user().await;
    let tenant_id = create_tenant(&app, "prune-null").await;
    let project_id = create_project(&app, tenant_id, "web").await;
    let db = &app.state.db;
    let storage = &app.state.storage;

    let b1 = make_passed_build(&app, tenant_id, project_id, "sha1").await;
    let b2 = make_passed_build(&app, tenant_id, project_id, "sha2").await;
    let b3 = make_passed_build(&app, tenant_id, project_id, "sha3").await;

    // 保持数は未設定（NULL = 無制限）。ベストエフォート版は何もしない。
    service::builds::prune_project_builds_best_effort(db, storage, project_id).await;

    assert_eq!(
        service::builds::count_builds(db, project_id)
            .await
            .expect("count"),
        3,
        "無制限なら 1 件も消えない"
    );
    for build in [&b1, &b2, &b3] {
        assert!(service::builds::get_build(db, build.0).await.is_ok());
        assert!(object_exists(storage, &build.2).await);
    }
}
