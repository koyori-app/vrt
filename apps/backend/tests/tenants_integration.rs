//! マルチテナント（テナント CRUD + ロールマトリクス）の統合テスト。
//!
//! 認可の取り決め:
//! - 非メンバーは存在有無を漏らさないため一律 403（task の `ensure_tenant_*` に合わせる）
//! - テナント管理系（作成 / 更新 / 削除 / メンバー操作）はセッション専用
//! - owner ロールの付与・剥奪は owner のみ。最後の owner は降格も削除もできない（409）

mod common;

use common::TestApp;
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// グローバル UNIQUE な slug を作る（DB は並列テストで共有される）。
fn unique_slug(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().to_string()[..8])
}

/// テナントを作成して (id, slug) を返す。
async fn create_tenant(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let slug = unique_slug(prefix);
    let response = app
        .post_json("/v1/tenants", json!({ "name": "Acme", "slug": slug }))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: serde_json::Value = response.json().await.expect("tenant json");
    assert_eq!(body["slug"].as_str(), Some(slug.as_str()));
    let id = Uuid::parse_str(body["id"].as_str().expect("tenant id")).expect("uuid");
    (id, slug)
}

async fn member_role(app: &TestApp, tenant_id: Uuid, user_id: Uuid) -> Option<String> {
    let response = app.get(&format!("/v1/tenants/{tenant_id}/members")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("members json");
    body.as_array()
        .expect("array")
        .iter()
        .find(|m| m["user_id"].as_str() == Some(&user_id.to_string()))
        .map(|m| m["role"].as_str().expect("role").to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn tenant_crud_and_creator_becomes_owner() {
    let app = TestApp::new().await;
    let user = app.login_as_new_user().await;

    let (tenant_id, slug) = create_tenant(&app, "crud").await;

    // 作成者は owner
    assert_eq!(
        member_role(&app, tenant_id, user.id).await.as_deref(),
        Some("owner")
    );

    // 一覧に出る
    let list = app.get("/v1/tenants").await;
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = list.json().await.expect("list json");
    assert!(
        body.as_array()
            .expect("array")
            .iter()
            .any(|t| t["slug"].as_str() == Some(slug.as_str()))
    );

    // 単体取得
    let get = app.get(&format!("/v1/tenants/{tenant_id}")).await;
    assert_eq!(get.status(), StatusCode::OK);

    // 更新（owner は admin 以上なので通る）
    let patch = app
        .patch_json(
            &format!("/v1/tenants/{tenant_id}"),
            json!({ "name": "Renamed" }),
        )
        .await;
    assert_eq!(patch.status(), StatusCode::OK);
    let body: serde_json::Value = patch.json().await.expect("patch json");
    assert_eq!(body["name"].as_str(), Some("Renamed"));
    assert_eq!(body["slug"].as_str(), Some(slug.as_str()), "slug は不変");

    // 削除（owner のみ）→ 以降は非メンバー扱いで 403
    let delete = app.delete(&format!("/v1/tenants/{tenant_id}")).await;
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/v1/tenants/{tenant_id}")).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slug_is_validated_and_globally_unique() {
    let app = TestApp::new().await;
    app.login_as_new_user().await;

    let (_, slug) = create_tenant(&app, "dup").await;

    // 同じ slug は 409
    let conflict = app
        .post_json("/v1/tenants", json!({ "name": "Other", "slug": slug }))
        .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    // 書式違反は 400
    for bad in ["Bad Slug", "UPPER", "-lead", "trail-", "under_score", "a"] {
        let response = app
            .post_json("/v1/tenants", json!({ "name": "Bad", "slug": bad }))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "slug {bad:?} should be rejected"
        );
    }

    // 予約語は 400
    let reserved = app
        .post_json("/v1/tenants", json!({ "name": "Bad", "slug": "admin" }))
        .await;
    assert_eq!(reserved.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn non_member_is_denied_and_sees_nothing() {
    let owner_app = TestApp::new().await;
    owner_app.login_as_new_user().await;
    let (tenant_id, slug) = create_tenant(&owner_app, "hidden").await;

    let outsider = TestApp::new().await;
    outsider.login_as_new_user().await;

    // 存在有無を漏らさないため、参照も更新も削除も 403
    assert_eq!(
        outsider
            .get(&format!("/v1/tenants/{tenant_id}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        outsider
            .patch_json(&format!("/v1/tenants/{tenant_id}"), json!({ "name": "x" }))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        outsider
            .delete(&format!("/v1/tenants/{tenant_id}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        outsider
            .get(&format!("/v1/tenants/{tenant_id}/members"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // 存在しないテナントも同じ 403（レスポンスから存在有無が判別できない）
    assert_eq!(
        outsider
            .get(&format!("/v1/tenants/{}", Uuid::new_v4()))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // 一覧にも出ない
    let list = outsider.get("/v1/tenants").await;
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = list.json().await.expect("list json");
    assert!(
        !body
            .as_array()
            .expect("array")
            .iter()
            .any(|t| t["slug"].as_str() == Some(slug.as_str()))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn role_matrix_governs_member_management() {
    let owner_app = TestApp::new().await;
    let owner = owner_app.login_as_new_user().await;
    let (tenant_id, _) = create_tenant(&owner_app, "roles").await;

    let member_app = TestApp::new().await;
    let member = member_app.login_as_new_user().await;

    let third_app = TestApp::new().await;
    let third = third_app.login_as_new_user().await;

    // owner が member を追加（username 指定）
    let add = owner_app
        .post_json(
            &format!("/v1/tenants/{tenant_id}/members"),
            json!({ "username": member.username, "role": "member" }),
        )
        .await;
    assert_eq!(add.status(), StatusCode::CREATED);

    // 二重追加は 409
    assert_eq!(
        owner_app
            .post_json(
                &format!("/v1/tenants/{tenant_id}/members"),
                json!({ "user_id": member.id, "role": "member" }),
            )
            .await
            .status(),
        StatusCode::CONFLICT
    );

    // member はテナント更新もメンバー追加もできない
    assert_eq!(
        member_app
            .patch_json(
                &format!("/v1/tenants/{tenant_id}"),
                json!({ "name": "nope" })
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        member_app
            .post_json(
                &format!("/v1/tenants/{tenant_id}/members"),
                json!({ "user_id": third.id, "role": "member" }),
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // member でも自分のテナント一覧・詳細は見える
    assert_eq!(
        member_app
            .get(&format!("/v1/tenants/{tenant_id}"))
            .await
            .status(),
        StatusCode::OK
    );

    // owner が member を admin に昇格
    let promote = owner_app
        .patch_json(
            &format!("/v1/tenants/{tenant_id}/members/{}", member.id),
            json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(promote.status(), StatusCode::OK);

    // admin はメンバー追加ができる
    assert_eq!(
        member_app
            .post_json(
                &format!("/v1/tenants/{tenant_id}/members"),
                json!({ "user_id": third.id, "role": "member" }),
            )
            .await
            .status(),
        StatusCode::CREATED
    );

    // admin は owner を付与できない
    assert_eq!(
        member_app
            .patch_json(
                &format!("/v1/tenants/{tenant_id}/members/{}", third.id),
                json!({ "role": "owner" }),
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // admin は owner を剥奪できない / 追い出せない
    assert_eq!(
        member_app
            .patch_json(
                &format!("/v1/tenants/{tenant_id}/members/{}", owner.id),
                json!({ "role": "member" }),
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        member_app
            .delete(&format!("/v1/tenants/{tenant_id}/members/{}", owner.id))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // owner だけが owner を付与できる（= 所有権の移譲）
    assert_eq!(
        owner_app
            .patch_json(
                &format!("/v1/tenants/{tenant_id}/members/{}", member.id),
                json!({ "role": "owner" }),
            )
            .await
            .status(),
        StatusCode::OK
    );
    // owner が 2 人になったので、元 owner は自分を降格できる
    assert_eq!(
        owner_app
            .patch_json(
                &format!("/v1/tenants/{tenant_id}/members/{}", owner.id),
                json!({ "role": "admin" }),
            )
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        member_role(&owner_app, tenant_id, member.id)
            .await
            .as_deref(),
        Some("owner")
    );

    // 自分自身の脱退は member でも可能
    assert_eq!(
        third_app
            .delete(&format!("/v1/tenants/{tenant_id}/members/{}", third.id))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        third_app
            .get(&format!("/v1/tenants/{tenant_id}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn last_owner_cannot_be_demoted_or_removed() {
    let app = TestApp::new().await;
    let owner = app.login_as_new_user().await;
    let (tenant_id, _) = create_tenant(&app, "lastowner").await;

    // 唯一の owner の降格は 409
    assert_eq!(
        app.patch_json(
            &format!("/v1/tenants/{tenant_id}/members/{}", owner.id),
            json!({ "role": "admin" }),
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );

    // 唯一の owner の削除（自分の脱退を含む）も 409
    assert_eq!(
        app.delete(&format!("/v1/tenants/{tenant_id}/members/{}", owner.id))
            .await
            .status(),
        StatusCode::CONFLICT
    );

    // ロールは変わっていない
    assert_eq!(
        member_role(&app, tenant_id, owner.id).await.as_deref(),
        Some("owner")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tenant_management_is_session_only() {
    let app = TestApp::new().await;
    let user = app.login_as_new_user().await;
    let (tenant_id, _) = create_tenant(&app, "patguard").await;

    let (token, _) = app
        .insert_personal_token(user.id, vec![entity::scopes::Scope::ReadProject])
        .await;

    // 参照は read:project を持つ PAT でも通る
    assert_eq!(
        app.get_with_bearer(&format!("/v1/tenants/{tenant_id}"), &token)
            .await
            .status(),
        StatusCode::OK
    );

    // 作成・メンバー操作はセッション専用
    assert_eq!(
        app.post_json_with_bearer(
            "/v1/tenants",
            &token,
            json!({ "name": "Nope", "slug": unique_slug("pat") }),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.post_json_with_bearer(
            &format!("/v1/tenants/{tenant_id}/members"),
            &token,
            json!({ "user_id": user.id, "role": "member" }),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_carries_my_role_and_members_carry_user_profiles() {
    // 別ユーザーを 1 人用意して、member として招く。
    let other_app = TestApp::new().await;
    let other = other_app.login_as_new_user().await;

    let app = TestApp::new().await;
    let owner = app.login_as_new_user().await;
    let (tenant_id, slug) = create_tenant(&app, "profiles").await;

    let added = app
        .post_json(
            &format!("/v1/tenants/{tenant_id}/members"),
            json!({ "user_id": other.id, "role": "member" }),
        )
        .await;
    assert_eq!(added.status(), StatusCode::CREATED);
    let added: serde_json::Value = added.json().await.expect("member json");
    // 追加レスポンスは join 前の形なので username は入らない（一覧が唯一の供給元）。
    assert_eq!(added["role"].as_str(), Some("member"));

    // 一覧は呼び出し元のロールを含む。
    let list = app.get("/v1/tenants").await;
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = list.json().await.expect("list json");
    let mine = body
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["slug"].as_str() == Some(slug.as_str()))
        .expect("own tenant in list");
    assert_eq!(mine["my_role"].as_str(), Some("owner"));

    // 単体取得も同じロールを返す。
    let get = app.get(&format!("/v1/tenants/{tenant_id}")).await;
    assert_eq!(get.status(), StatusCode::OK);
    let body: serde_json::Value = get.json().await.expect("tenant json");
    assert_eq!(body["my_role"].as_str(), Some("owner"));

    // 招かれた側から見た my_role は member。
    let list = other_app.get("/v1/tenants").await;
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = list.json().await.expect("list json");
    let theirs = body
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["slug"].as_str() == Some(slug.as_str()))
        .expect("invited tenant in list");
    assert_eq!(theirs["my_role"].as_str(), Some("member"));

    // メンバー一覧は users を join して表示名を返す。
    let members = app.get(&format!("/v1/tenants/{tenant_id}/members")).await;
    assert_eq!(members.status(), StatusCode::OK);
    let body: serde_json::Value = members.json().await.expect("members json");
    let members = body.as_array().expect("array");
    assert_eq!(members.len(), 2);

    for (user, expected_role) in [(&owner, "owner"), (&other, "member")] {
        let row = members
            .iter()
            .find(|m| m["user_id"].as_str() == Some(&user.id.to_string()))
            .unwrap_or_else(|| panic!("member row for {}", user.username));
        assert_eq!(row["role"].as_str(), Some(expected_role));
        assert_eq!(row["username"].as_str(), Some(user.username.as_str()));
        assert_eq!(
            row["display_name"].as_str(),
            Some(user.display_name.as_str())
        );
        assert_eq!(row["avatar_url"].as_str(), user.avatar_url.as_deref());
    }
}
