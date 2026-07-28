//! プロジェクト CRUD・テナント越境・PAT スコープの統合テスト。

mod common;

use common::TestApp;
use entity::scopes::Scope;
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

fn unique_slug(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().to_string()[..8])
}

async fn create_tenant(app: &TestApp, prefix: &str) -> Uuid {
    let response = app
        .post_json(
            "/v1/tenants",
            json!({ "name": "Acme", "slug": unique_slug(prefix) }),
        )
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

#[tokio::test(flavor = "multi_thread")]
async fn project_crud_within_tenant() {
    let app = TestApp::new().await;
    app.login_as_new_user().await;
    let tenant_id = create_tenant(&app, "proj").await;

    // 作成（既定値が入る）
    let created = app
        .post_json(
            &format!("/v1/tenants/{tenant_id}/projects"),
            json!({ "name": "Web", "slug": "web" }),
        )
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let body: serde_json::Value = created.json().await.expect("project json");
    let project_id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    assert_eq!(
        body["tenant_id"].as_str(),
        Some(tenant_id.to_string().as_str())
    );
    assert_eq!(body["default_branch"].as_str(), Some("main"));
    assert_eq!(body["diff_threshold"].as_f64(), Some(0.1));
    assert_eq!(body["diff_ratio_fail"].as_f64(), Some(0.0));
    assert!(body["github_installation_id"].is_null());
    assert!(body["github_repo"].is_null());

    // 一覧
    let list = app.get(&format!("/v1/tenants/{tenant_id}/projects")).await;
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = list.json().await.expect("list json");
    assert_eq!(body.as_array().expect("array").len(), 1);

    // 単体取得
    let get = app.get(&format!("/v1/projects/{project_id}")).await;
    assert_eq!(get.status(), StatusCode::OK);

    // 設定更新
    let patch = app
        .patch_json(
            &format!("/v1/projects/{project_id}"),
            json!({
                "name": "Web App",
                "default_branch": "develop",
                "diff_threshold": 0.25,
                "diff_ratio_fail": 0.5,
            }),
        )
        .await;
    assert_eq!(patch.status(), StatusCode::OK);
    let body: serde_json::Value = patch.json().await.expect("patch json");
    assert_eq!(body["name"].as_str(), Some("Web App"));
    assert_eq!(body["default_branch"].as_str(), Some("develop"));
    assert_eq!(body["diff_threshold"].as_f64(), Some(0.25));
    assert_eq!(body["diff_ratio_fail"].as_f64(), Some(0.5));

    // 削除（owner）
    assert_eq!(
        app.delete(&format!("/v1/projects/{project_id}"))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.get(&format!("/v1/projects/{project_id}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_slug_is_unique_per_tenant_and_validated() {
    let app = TestApp::new().await;
    app.login_as_new_user().await;
    let tenant_a = create_tenant(&app, "slug-a").await;
    let tenant_b = create_tenant(&app, "slug-b").await;

    create_project(&app, tenant_a, "web").await;

    // 同一テナント内の重複は 409
    let conflict = app
        .post_json(
            &format!("/v1/tenants/{tenant_a}/projects"),
            json!({ "name": "Web again", "slug": "web" }),
        )
        .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    // 別テナントでは同じ slug を使える
    create_project(&app, tenant_b, "web").await;

    // 書式違反・予約語は 400
    for bad in ["Bad Slug", "UPPER", "--x--", "api"] {
        assert_eq!(
            app.post_json(
                &format!("/v1/tenants/{tenant_a}/projects"),
                json!({ "name": "Bad", "slug": bad }),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "slug {bad:?} should be rejected"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_threshold_out_of_range_is_rejected() {
    let app = TestApp::new().await;
    app.login_as_new_user().await;
    let tenant_id = create_tenant(&app, "range").await;
    let project_id = create_project(&app, tenant_id, "web").await;

    for body in [
        json!({ "diff_threshold": 1.5 }),
        json!({ "diff_threshold": -0.1 }),
        json!({ "diff_ratio_fail": 1.5 }),
    ] {
        assert_eq!(
            app.patch_json(&format!("/v1/projects/{project_id}"), body.clone())
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "{body} should be rejected"
        );
    }

    // 境界値は通る
    assert_eq!(
        app.patch_json(
            &format!("/v1/projects/{project_id}"),
            json!({ "diff_threshold": 1.0, "diff_ratio_fail": 0.0 }),
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_tenant_project_access_is_denied() {
    let owner_app = TestApp::new().await;
    owner_app.login_as_new_user().await;
    let tenant_id = create_tenant(&owner_app, "xtenant").await;
    let project_id = create_project(&owner_app, tenant_id, "web").await;

    let outsider = TestApp::new().await;
    outsider.login_as_new_user().await;
    // 部外者は自分のテナントを持っていても、他テナントのプロジェクトには触れない
    create_tenant(&outsider, "other").await;

    assert_eq!(
        outsider
            .get(&format!("/v1/projects/{project_id}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        outsider
            .patch_json(
                &format!("/v1/projects/{project_id}"),
                json!({ "name": "x" })
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        outsider
            .delete(&format!("/v1/projects/{project_id}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        outsider
            .get(&format!("/v1/tenants/{tenant_id}/projects"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // 存在しないプロジェクト ID も同じ 403（存在有無を漏らさない）
    assert_eq!(
        outsider
            .get(&format!("/v1/projects/{}", Uuid::new_v4()))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_writes_require_admin_or_owner() {
    let owner_app = TestApp::new().await;
    owner_app.login_as_new_user().await;
    let tenant_id = create_tenant(&owner_app, "pwrite").await;
    let project_id = create_project(&owner_app, tenant_id, "web").await;

    let member_app = TestApp::new().await;
    let member = member_app.login_as_new_user().await;
    assert_eq!(
        owner_app
            .post_json(
                &format!("/v1/tenants/{tenant_id}/members"),
                json!({ "user_id": member.id, "role": "member" }),
            )
            .await
            .status(),
        StatusCode::CREATED
    );

    // member は参照だけ
    assert_eq!(
        member_app
            .get(&format!("/v1/tenants/{tenant_id}/projects"))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        member_app
            .get(&format!("/v1/projects/{project_id}"))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        member_app
            .post_json(
                &format!("/v1/tenants/{tenant_id}/projects"),
                json!({ "name": "Nope", "slug": "nope" }),
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        member_app
            .patch_json(
                &format!("/v1/projects/{project_id}"),
                json!({ "name": "x" })
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // admin に昇格すると作成・更新はできるが、削除は owner 専用
    assert_eq!(
        owner_app
            .patch_json(
                &format!("/v1/tenants/{tenant_id}/members/{}", member.id),
                json!({ "role": "admin" }),
            )
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        member_app
            .post_json(
                &format!("/v1/tenants/{tenant_id}/projects"),
                json!({ "name": "Docs", "slug": "docs-site" }),
            )
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        member_app
            .patch_json(
                &format!("/v1/projects/{project_id}"),
                json!({ "name": "x" })
            )
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        member_app
            .delete(&format!("/v1/projects/{project_id}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pat_read_project_scope_gates_project_reads() {
    let app = TestApp::new().await;
    let user = app.login_as_new_user().await;
    let tenant_id = create_tenant(&app, "patproj").await;
    let project_id = create_project(&app, tenant_id, "web").await;

    let (reader, _) = app
        .insert_personal_token(user.id, vec![Scope::ReadProject])
        .await;
    let (ci_only, _) = app
        .insert_personal_token(user.id, vec![Scope::WriteBuild])
        .await;

    // read:project を持つ PAT は参照できる
    assert_eq!(
        app.get_with_bearer(&format!("/v1/tenants/{tenant_id}/projects"), &reader)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.get_with_bearer(&format!("/v1/projects/{project_id}"), &reader)
            .await
            .status(),
        StatusCode::OK
    );

    // write:build だけの PAT は read:project ゲートを通れない
    assert_eq!(
        app.get_with_bearer(&format!("/v1/tenants/{tenant_id}/projects"), &ci_only)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get_with_bearer(&format!("/v1/projects/{project_id}"), &ci_only)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // 作成はスコープに関わらずセッション専用
    assert_eq!(
        app.post_json_with_bearer(
            &format!("/v1/tenants/{tenant_id}/projects"),
            &reader,
            json!({ "name": "Nope", "slug": "nope" }),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}
