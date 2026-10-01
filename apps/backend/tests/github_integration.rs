//! GitHub App 連携の統合テスト（Phase 6 の受け入れゲート）。
//!
//! GitHub API は wiremock で偽装する（`Settings::github_api_base_url` で差し替え）。
//! webhook は本物の HMAC 署名を付けて `POST /v1/github/webhook` に投げ、
//! ワーカー経由で `github_installations` に反映されることまで確認する。
//!
//! シナリオ:
//!
//! 1. webhook: 正しい署名の `installation.created` で行ができる / 壊れた署名は 401 で行ができない
//! 2. webhook: `installation.deleted` で `deleted_at` が入り、紐付いたプロジェクトが解除される
//! 3. claim: admin は claim できる / member は 403 / 他テナントが claim 済みなら 409
//! 4. プロジェクト紐付け + CI ビルド → finalize で pending、比較完了で pending（差分あり）、
//!    承認で success のコミットステータスが wiremock に届く
//! 5. GitHub App 未設定でもビルドフローは完走し、ステータス POST は起きない

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use apalis::prelude::Data;
use common::{TEST_INSTALLATION_TOKEN, TestApp};
use entity::scopes::Scope;
use image::{Rgba, RgbaImage};
use reqwest::StatusCode;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{Mock, ResponseTemplate, matchers::method, matchers::path, matchers::query_param};

/// ワーカー（webhook / status / compare）の処理待ちタイムアウト。
const POLL_TIMEOUT: Duration = Duration::from_secs(60);

// ── ヘルパー ────────────────────────────────────────────────────────────

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let image = RgbaImage::from_pixel(width, height, Rgba(color));
    let mut buf = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}

/// テストごとに衝突しない installation ID（DB はプロセス内で共有される）。
fn unique_installation_id() -> i64 {
    let bytes = Uuid::new_v4();
    i64::from(u32::from_be_bytes(
        bytes.as_bytes()[..4].try_into().unwrap(),
    )) + 1_000_000
}

fn installation_payload(action: &str, installation_id: i64, login: &str) -> Value {
    json!({
        "action": action,
        "installation": {
            "id": installation_id,
            "account": { "login": login, "type": "Organization" },
        },
    })
}

/// `/installation/repositories` の 1 ページ分のレスポンス。
fn repository_page(start: usize, count: usize, total_count: u64) -> Value {
    let repositories: Vec<Value> = (0..count)
        .map(|offset| {
            let n = start + offset;
            json!({
                "id": n as i64,
                "name": format!("repo-{n:05}"),
                "full_name": format!("acme-inc/repo-{n:05}"),
                "private": false,
                "archived": false,
                "html_url": format!("https://github.com/acme-inc/repo-{n:05}"),
            })
        })
        .collect();
    json!({ "total_count": total_count, "repositories": repositories })
}

/// テナント + プロジェクト + claim 済み installation を用意する。
async fn claimed_installation(app: &TestApp) -> (Project, i64) {
    app.login_as_new_user().await;
    let project = create_tenant_and_project(app).await;
    let installation_id = unique_installation_id();

    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;

    let state = issue_setup_state(app, &project.tenant_id).await;
    let res = app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": state }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "claim installation");
    (project, installation_id)
}

/// claim に必要な one-time state を発行する（admin 以上でないと 403）。
async fn issue_setup_state(app: &TestApp, tenant_id: &str) -> String {
    let res = app
        .post_json("/v1/github/setup/state", json!({ "tenant_id": tenant_id }))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "issue setup state");
    let body: Value = res.json().await.expect("setup state json");
    body["state"].as_str().expect("state").to_string()
}

/// テナント + プロジェクトを作り、ID / slug を返す。
struct Project {
    tenant_id: String,
    tenant_slug: String,
    project_id: String,
    project_slug: String,
}

async fn create_tenant_and_project(app: &TestApp) -> Project {
    let suffix = Uuid::new_v4().simple().to_string();
    let tenant_slug = format!("gh-{}", &suffix[..8]);
    let project_slug = format!("web-{}", &suffix[8..16]);

    let res = app
        .post_json(
            "/v1/tenants",
            json!({ "name": "GitHub Co", "slug": tenant_slug }),
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

    Project {
        tenant_id,
        tenant_slug,
        project_id: project["id"].as_str().expect("project id").to_string(),
        project_slug,
    }
}

async fn create_pr_build(
    app: &TestApp,
    project: &Project,
    token: &str,
    sha: &str,
    pr_number: i32,
) -> Value {
    let response = app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                project.tenant_slug, project.project_slug
            ),
            token,
            json!({
                "branch": "feature/stale-comment",
                "commit_sha": sha,
                "pull_request_number": pr_number,
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED, "create PR build");
    response.json().await.expect("build json")
}

// ── 1. webhook の署名検証 ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn webhook_with_valid_signature_creates_installation_row() {
    let app = TestApp::new_with_github().await;
    let installation_id = unique_installation_id();

    let res = app
        .post_github_webhook(
            "installation",
            &installation_payload("created", installation_id, "acme-inc"),
            None,
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::ACCEPTED,
        "valid webhook should be accepted"
    );

    let row = app
        .wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;
    assert_eq!(row.installation_id, installation_id);
    assert_eq!(row.account_login, "acme-inc");
    assert_eq!(row.account_type, "Organization");
    // claim されるまでテナントには紐付かない。
    assert!(row.tenant_id.is_none(), "installation starts unclaimed");
    assert!(row.deleted_at.is_none());
    assert!(row.suspended_at.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_with_bad_signature_is_rejected_and_writes_nothing() {
    let app = TestApp::new_with_github().await;
    let installation_id = unique_installation_id();
    let payload = installation_payload("created", installation_id, "evil-corp");

    // 別のシークレットで署名した（＝形式は正しいが一致しない）ケース。
    let body = serde_json::to_vec(&payload).expect("serialize");
    let forged = common::sign_webhook("not-the-real-secret", &body);
    let res = app
        .post_github_webhook("installation", &payload, Some(&forged))
        .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "forged signature");

    // 署名ヘッダそのものが無いケース。
    let res = app
        .post_github_webhook("installation", &payload, Some("garbage"))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "malformed signature"
    );

    // ジョブが投入されていないことを確かめるため、少し待ってから見る。
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        app.find_installation(installation_id).await.is_none(),
        "rejected webhook must not touch the database"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_ignores_unrelated_events() {
    let app = TestApp::new_with_github().await;
    let installation_id = unique_installation_id();

    // 購読していないイベントは 202 で受けて、ジョブ側が無視する。
    let res = app
        .post_github_webhook(
            "push",
            &json!({
                "ref": "refs/heads/main",
                "installation": { "id": installation_id },
            }),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        app.find_installation(installation_id).await.is_none(),
        "push event must not create an installation row"
    );
}

// ── 2. installation.deleted / suspend ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn installation_deleted_soft_deletes_row_and_unlinks_projects() {
    let app = TestApp::new_with_github().await;
    app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();

    // created → claim → プロジェクトに紐付け
    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;

    let res = app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&app, &project.tenant_id).await }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "claim installation");

    let res = app
        .patch_json(
            &format!("/v1/projects/{}/github", project.project_id),
            json!({ "installation_id": installation_id, "github_repo": "acme-inc/web" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "link project");
    let linked: Value = res.json().await.expect("project json");
    assert_eq!(
        linked["github_installation_id"].as_i64(),
        Some(installation_id)
    );
    assert_eq!(linked["github_repo"].as_str(), Some("acme-inc/web"));

    // アンインストール
    let res = app
        .post_github_webhook(
            "installation",
            &installation_payload("deleted", installation_id, "acme-inc"),
            None,
        )
        .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let row = app
        .wait_for_installation_where(installation_id, POLL_TIMEOUT, |row| {
            row.deleted_at.is_some()
        })
        .await;
    assert!(row.deleted_at.is_some(), "row is soft deleted");
    assert!(row.tenant_id.is_none(), "claim is released");

    // プロジェクト側の紐付けも外れる。
    let res = app
        .get(&format!("/v1/projects/{}", project.project_id))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let unlinked: Value = res.json().await.expect("project json");
    assert!(
        unlinked["github_installation_id"].is_null(),
        "project must be unlinked, got {unlinked}"
    );
    assert!(unlinked["github_repo"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn installation_suspend_and_unsuspend_toggle_suspended_at() {
    let app = TestApp::new_with_github().await;
    let installation_id = unique_installation_id();

    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;

    app.post_github_webhook(
        "installation",
        &installation_payload("suspend", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation_where(installation_id, POLL_TIMEOUT, |row| {
        row.suspended_at.is_some()
    })
    .await;

    app.post_github_webhook(
        "installation",
        &installation_payload("unsuspend", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation_where(installation_id, POLL_TIMEOUT, |row| {
        row.suspended_at.is_none()
    })
    .await;
}

// ── 3. claim フロー ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn claim_flow_enforces_roles_and_single_tenant_ownership() {
    let owner_app = TestApp::new_with_github().await;
    owner_app.login_as_new_user().await;
    let project = create_tenant_and_project(&owner_app).await;
    let installation_id = unique_installation_id();

    owner_app
        .post_github_webhook(
            "installation",
            &installation_payload("created", installation_id, "acme-inc"),
            None,
        )
        .await;
    owner_app
        .wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;

    // 未 claim 一覧に出る。
    let res = owner_app.get("/v1/github/installations/unclaimed").await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.expect("unclaimed json");
    let listed = body["installations"]
        .as_array()
        .expect("installations array")
        .iter()
        .any(|row| row["installation_id"].as_i64() == Some(installation_id));
    assert!(listed, "unclaimed installation should be listed");

    // member 権限のユーザーは claim できない。
    let member_app = TestApp::new_with_github().await;
    let member = member_app.login_as_new_user().await;
    let res = owner_app
        .post_json(
            &format!("/v1/tenants/{}/members", project.tenant_id),
            json!({ "user_id": member.id, "role": "member" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "add member");

    let res = member_app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": "not-a-real-state" }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "member must not be able to claim"
    );

    // admin（テナント作成者 = owner）は claim できる。
    let res = owner_app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&owner_app, &project.tenant_id).await }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "owner can claim");
    let claimed: Value = res.json().await.expect("claim json");
    assert_eq!(
        claimed["tenant_id"].as_str(),
        Some(project.tenant_id.as_str())
    );

    // 同じテナントの再 claim は冪等。
    let res = owner_app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&owner_app, &project.tenant_id).await }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "re-claim is idempotent");

    // claim 済み一覧に出る。
    let res = owner_app
        .get(&format!(
            "/v1/github/installations?tenant_id={}",
            project.tenant_id
        ))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.expect("installations json");
    assert_eq!(body["total"].as_u64(), Some(1));

    // 別テナントからの claim は 409。
    let other_app = TestApp::new_with_github().await;
    other_app.login_as_new_user().await;
    let other = create_tenant_and_project(&other_app).await;
    let res = other_app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": other.tenant_id, "state": issue_setup_state(&other_app, &other.tenant_id).await }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "installation already claimed by another tenant"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn claimed_installation_lists_accessible_repositories() {
    let app = TestApp::new_with_github().await;
    app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();

    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;
    let res = app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&app, &project.tenant_id).await }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let github = app.github.as_ref().expect("mock github");
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 2,
            "repositories": [
                {
                    "id": 2,
                    "name": "website",
                    "full_name": "acme-inc/website",
                    "private": true,
                    "archived": false,
                    "html_url": "https://github.com/acme-inc/website"
                },
                {
                    "id": 1,
                    "name": "design-system",
                    "full_name": "acme-inc/design-system",
                    "private": false,
                    "archived": false,
                    "html_url": "https://github.com/acme-inc/design-system"
                }
            ]
        })))
        .mount(&github.server)
        .await;

    let res = app
        .get(&format!(
            "/v1/github/installations/{installation_id}/repositories?tenant_id={}",
            project.tenant_id
        ))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.expect("repositories json");
    assert_eq!(body["total"].as_u64(), Some(2));
    assert_eq!(
        body["repositories"][0]["full_name"].as_str(),
        Some("acme-inc/design-system"),
        "repositories are sorted for the selector"
    );
}

/// repository 一覧は private repository の名前を含むので、テナントの一般メンバーには出さない。
#[tokio::test(flavor = "multi_thread")]
async fn repository_list_requires_admin() {
    let owner_app = TestApp::new_with_github().await;
    owner_app.login_as_new_user().await;
    let project = create_tenant_and_project(&owner_app).await;
    let installation_id = unique_installation_id();

    owner_app
        .post_github_webhook(
            "installation",
            &installation_payload("created", installation_id, "acme-inc"),
            None,
        )
        .await;
    owner_app
        .wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;
    let res = owner_app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&owner_app, &project.tenant_id).await }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "claim");

    let github = owner_app.github.as_ref().expect("mock github");
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 1,
            "repositories": [{
                "id": 1,
                "name": "secret",
                "full_name": "acme-inc/secret",
                "private": true,
                "archived": false,
                "html_url": "https://github.com/acme-inc/secret"
            }]
        })))
        .mount(&github.server)
        .await;

    // member を追加する。
    let member_app = TestApp::new_with_github().await;
    let member = member_app.login_as_new_user().await;
    let res = owner_app
        .post_json(
            &format!("/v1/tenants/{}/members", project.tenant_id),
            json!({ "user_id": member.id, "role": "member" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "add member");

    let url = format!(
        "/v1/github/installations/{installation_id}/repositories?tenant_id={}",
        project.tenant_id
    );
    let res = member_app.get(&url).await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "member must not see repository names"
    );

    let res = owner_app.get(&url).await;
    assert_eq!(res.status(), StatusCode::OK, "admin can list repositories");
}

/// `total_count` に届くまでページを辿る。
#[tokio::test(flavor = "multi_thread")]
async fn repository_list_follows_pagination() {
    let app = TestApp::new_with_github().await;
    let (project, installation_id) = claimed_installation(&app).await;
    let github = app.github.as_ref().expect("mock github");

    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repository_page(1, 100, 150)))
        .mount(&github.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repository_page(101, 50, 150)))
        .mount(&github.server)
        .await;

    let res = app
        .get(&format!(
            "/v1/github/installations/{installation_id}/repositories?tenant_id={}",
            project.tenant_id
        ))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.expect("repositories json");
    assert_eq!(body["total"].as_u64(), Some(150), "both pages are returned");
    assert_eq!(
        body["repositories"][149]["full_name"].as_str(),
        Some("acme-inc/repo-00150"),
        "the last page is included"
    );
}

/// ページ上限に達したら、部分結果を正常扱いせず明示的なエラーにする。
#[tokio::test(flavor = "multi_thread")]
async fn repository_list_errors_instead_of_truncating_at_the_page_limit() {
    let app = TestApp::new_with_github().await;
    let (project, installation_id) = claimed_installation(&app).await;
    let github = app.github.as_ref().expect("mock github");

    // total_count に永遠に届かないレスポンス（= 10,000 件を超える installation）。
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repository_page(1, 100, 50_000)))
        .mount(&github.server)
        .await;

    let res = app
        .get(&format!(
            "/v1/github/installations/{installation_id}/repositories?tenant_id={}",
            project.tenant_id
        ))
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "truncated list must not be returned as success"
    );
    let body: Value = res.json().await.expect("error json");
    let message = body.to_string();
    assert!(
        message.contains("page limit") && message.contains("50000"),
        "error should say how much was fetched, got {message}"
    );
}

/// レート制限（429 / 403 + X-RateLimit-Remaining: 0）は一時エラーとして扱う。
#[tokio::test(flavor = "multi_thread")]
async fn repository_list_treats_rate_limits_as_transient() {
    for (status, headers) in [
        (429_u16, vec![("Retry-After", "60")]),
        (
            403_u16,
            vec![("X-RateLimit-Remaining", "0"), ("X-RateLimit-Reset", "1")],
        ),
    ] {
        let app = TestApp::new_with_github().await;
        let (project, installation_id) = claimed_installation(&app).await;
        let github = app.github.as_ref().expect("mock github");

        let mut response = ResponseTemplate::new(status).set_body_string("{\"message\":\"limit\"}");
        for (name, value) in headers {
            response = response.insert_header(name, value);
        }
        Mock::given(method("GET"))
            .and(path("/installation/repositories"))
            .respond_with(response)
            .mount(&github.server)
            .await;

        let res = app
            .get(&format!(
                "/v1/github/installations/{installation_id}/repositories?tenant_id={}",
                project.tenant_id
            ))
            .await;
        assert_eq!(
            res.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "rate limited response ({status}) must not become a 400"
        );
    }
}

/// レート制限ではない 403 は従来どおり永続エラー（400）。
#[tokio::test(flavor = "multi_thread")]
async fn repository_list_keeps_plain_forbidden_permanent() {
    let app = TestApp::new_with_github().await;
    let (project, installation_id) = claimed_installation(&app).await;
    let github = app.github.as_ref().expect("mock github");

    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(
            ResponseTemplate::new(403).set_body_string("{\"message\":\"Resource not accessible\"}"),
        )
        .mount(&github.server)
        .await;

    let res = app
        .get(&format!(
            "/v1/github/installations/{installation_id}/repositories?tenant_id={}",
            project.tenant_id
        ))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

/// state は最初に使われた installation に予約され、別の installation には使えない。
///
/// 検証と消費が分かれていると、並行リクエストが予約前の state を 2 本とも読んで
/// 別々の installation を claim できてしまう。予約は claim の成否に関わらず
/// 最初の 1 回で確定するので、claim が失敗する installation で予約してから確かめる。
#[tokio::test(flavor = "multi_thread")]
async fn setup_state_is_bound_to_the_first_installation() {
    let app = TestApp::new_with_github().await;
    app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;

    // DB に存在しない installation。claim は 404 になるが、state はここに予約される。
    let absent_installation_id = unique_installation_id();
    let state = issue_setup_state(&app, &project.tenant_id).await;

    let res = app
        .post_json(
            &format!("/v1/github/installations/{absent_installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": state }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "claim fails, but the state is now reserved"
    );

    // 同じ installation での再試行は許す（webhook 到着待ちのリトライ経路）。
    let res = app
        .post_json(
            &format!("/v1/github/installations/{absent_installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": state }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "retrying the same installation must not be rejected as a state conflict"
    );

    // 実在する別の installation を用意する。
    let other_installation_id = unique_installation_id();
    app.post_github_webhook(
        "installation",
        &installation_payload("created", other_installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(other_installation_id, POLL_TIMEOUT)
        .await;

    // 予約済みの state は、別の installation には使えない。
    let res = app
        .post_json(
            &format!("/v1/github/installations/{other_installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": state }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "a state held by another installation must not claim"
    );

    // 新しく発行した state なら通る。
    let fresh_state = issue_setup_state(&app, &project.tenant_id).await;
    let res = app
        .post_json(
            &format!("/v1/github/installations/{other_installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": fresh_state }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "a fresh state claims");
}

/// consume は state 本体と予約キーを 1 コマンドで原子的に消し、消費した値を返す。
///
/// claim の DB 更新が終わってから呼ばれるため、state だけ消えて予約キーが残る／
/// あるいは値を返しそこねる、といった部分失敗があると再試行できないゴミが残る。
/// service の `consume_setup_state` を直接叩き、両キーが消えること・2 回目が
/// `None` になることを Valkey で確かめる。
#[tokio::test(flavor = "multi_thread")]
async fn consume_setup_state_atomically_clears_state_and_holder() {
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;

    let redis = &app.state.redis_client;
    let tenant_id = Uuid::parse_str(&project.tenant_id).expect("tenant uuid");

    // state 本体と、reserve で作られる予約キーの両方を用意する。
    let state = service::github::issue_setup_state(redis, user.id, tenant_id)
        .await
        .expect("issue setup state");
    let installation_id = unique_installation_id();
    match service::github::reserve_setup_state(redis, &state, installation_id)
        .await
        .expect("reserve setup state")
    {
        service::github::SetupStateReservation::Reserved(reserved) => {
            assert_eq!(reserved.user_id, user.id);
            assert_eq!(reserved.tenant_id, tenant_id);
        }
        other => panic!("expected the state to be reservable, got {other:?}"),
    }

    // これらは service 側の非公開プレフィックスと一致させる（Valkey 上のキー名の契約）。
    let state_key = format!("github:setup_state:{state}");
    let holder_key = format!("github:setup_state_holder:{state}");

    // 消費すると、予約時に埋めた発行元がそのまま返る。
    let consumed = service::github::consume_setup_state(redis, &state)
        .await
        .expect("consume setup state")
        .expect("state is present on first consume");
    assert_eq!(consumed.user_id, user.id);
    assert_eq!(consumed.tenant_id, tenant_id);

    // state 本体も予約キーも残さない。
    let mut conn = redis.conn.acquire().await.expect("redis acquire");
    let remaining: i64 = redis::cmd("EXISTS")
        .arg(&state_key)
        .arg(&holder_key)
        .query_async(&mut *conn)
        .await
        .expect("redis exists");
    assert_eq!(
        remaining, 0,
        "consume must clear both the state and holder keys"
    );

    // 2 回目は消費済みなので None。
    let again = service::github::consume_setup_state(redis, &state)
        .await
        .expect("consume setup state again");
    assert!(
        again.is_none(),
        "a consumed state must not be consumable twice"
    );
}

/// claim は「発行者・テナントが一致する、未使用かつ期限内の state」でしか通らない。
#[tokio::test(flavor = "multi_thread")]
async fn claim_rejects_tampered_foreign_and_reused_setup_state() {
    let app = TestApp::new_with_github().await;
    app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();

    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;

    let claim_url = format!("/v1/github/installations/{installation_id}/claim");

    // 1. 存在しない（＝攻撃者が捏造した / 期限切れの）state は通らない。
    let res = app
        .post_json(
            &claim_url,
            json!({ "tenant_id": project.tenant_id, "state": "forged-state-value" }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "unknown state must be rejected"
    );

    // 2. 別ユーザーが自分のテナント向けに発行した state は流用できない。
    let other_app = TestApp::new_with_github().await;
    other_app.login_as_new_user().await;
    let other = create_tenant_and_project(&other_app).await;
    let foreign_state = issue_setup_state(&other_app, &other.tenant_id).await;
    let res = app
        .post_json(
            &claim_url,
            json!({ "tenant_id": project.tenant_id, "state": foreign_state }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "state issued for another user/tenant must be rejected"
    );

    // 3. member は state を発行できない。
    let member_app = TestApp::new_with_github().await;
    let member = member_app.login_as_new_user().await;
    let res = app
        .post_json(
            &format!("/v1/tenants/{}/members", project.tenant_id),
            json!({ "user_id": member.id, "role": "member" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "add member");
    let res = member_app
        .post_json(
            "/v1/github/setup/state",
            json!({ "tenant_id": project.tenant_id }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "member must not issue a setup state"
    );

    // 4. 正しい state は 1 回だけ通る（成功時に消費される）。
    let state = issue_setup_state(&app, &project.tenant_id).await;
    let res = app
        .post_json(
            &claim_url,
            json!({ "tenant_id": project.tenant_id, "state": state }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "valid state claims");

    let res = app
        .post_json(
            &claim_url,
            json!({ "tenant_id": project.tenant_id, "state": state }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "state cannot be replayed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_cannot_be_linked_to_foreign_or_malformed_repository() {
    let app = TestApp::new_with_github().await;
    app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();

    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;

    // まだ claim されていない installation には紐付けられない。
    let res = app
        .patch_json(
            &format!("/v1/projects/{}/github", project.project_id),
            json!({ "installation_id": installation_id, "github_repo": "acme-inc/web" }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "unclaimed installation belongs to no tenant"
    );

    app.post_json(
        &format!("/v1/github/installations/{installation_id}/claim"),
        json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&app, &project.tenant_id).await }),
    )
    .await;

    // リポジトリ形式が不正。
    let res = app
        .patch_json(
            &format!("/v1/projects/{}/github", project.project_id),
            json!({ "installation_id": installation_id, "github_repo": "not-a-repo" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "malformed repo");

    // 片方だけの指定も不正。
    let res = app
        .patch_json(
            &format!("/v1/projects/{}/github", project.project_id),
            json!({ "installation_id": installation_id }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "half-specified link");

    // 正常系 → 解除。
    let res = app
        .patch_json(
            &format!("/v1/projects/{}/github", project.project_id),
            json!({ "installation_id": installation_id, "github_repo": "acme-inc/web" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .patch_json(
            &format!("/v1/projects/{}/github", project.project_id),
            json!({ "installation_id": null, "github_repo": null }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "unlink");
    let unlinked: Value = res.json().await.expect("project json");
    assert!(unlinked["github_installation_id"].is_null());
}

// ── 4. コミットステータスの投稿 ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn build_lifecycle_posts_commit_statuses_to_github() {
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();
    let repo = "acme-inc/web";
    let sha = Uuid::new_v4().simple().to_string();

    app.github().expect_commit_statuses(repo, &sha).await;
    // 既存コメントは無い → 作成（POST）経路。
    app.github().expect_pr_comments(repo, 7, json!([])).await;

    // installation を作って claim し、プロジェクトに紐付ける。
    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;
    let res = app
        .post_json(
            &format!("/v1/github/installations/{installation_id}/claim"),
            json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&app, &project.tenant_id).await }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "claim");
    let res = app
        .patch_json(
            &format!("/v1/projects/{}/github", project.project_id),
            json!({ "installation_id": installation_id, "github_repo": repo }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "link project to repo");

    // CI ビルドを 1 本流す。
    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;

    let res = app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                project.tenant_slug, project.project_slug
            ),
            &token,
            json!({ "branch": "feature/x", "commit_sha": sha, "pull_request_number": 7 }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "create build");
    let build: Value = res.json().await.expect("build json");
    let build_id: Uuid = build["id"].as_str().expect("build id").parse().unwrap();
    let number = build["number"].as_i64().expect("build number");

    let status = app
        .upload_screenshot(build_id, &token, "home", png(8, 8, [255, 0, 0, 255]))
        .await
        .status();
    assert_eq!(status, StatusCode::CREATED, "upload screenshot");

    let status = app
        .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &token)
        .await
        .status();
    assert_eq!(status, StatusCode::OK, "finalize");

    // finalize 直後は pending（queued）。
    let pending = app
        .github()
        .wait_for_status(repo, &sha, "pending", POLL_TIMEOUT)
        .await;
    assert_eq!(pending["context"].as_str(), Some("vrt"));
    assert_eq!(
        pending["target_url"].as_str(),
        Some(
            format!(
                "{}/t/{}/p/{}/builds/{number}",
                app.base_url(),
                project.tenant_slug,
                project.project_slug
            )
            .as_str()
        ),
        "target_url points at the frontend build page"
    );

    // 初回ビルドなので baseline が無く、全部 added → changes_detected（＝レビュー待ちの pending）。
    let build = wait_for_terminal(&app, build_id, &token).await;
    assert_eq!(build["status"].as_str(), Some("changes_detected"));

    // 承認すると success になる。
    let res = app
        .post_json(
            &format!("/v1/builds/{build_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve build");

    let success = app
        .github()
        .wait_for_status(repo, &sha, "success", POLL_TIMEOUT)
        .await;
    assert_eq!(success["context"].as_str(), Some("vrt"));
    assert_eq!(
        success["description"].as_str(),
        Some("Visual changes approved")
    );

    // PR にはビルドリンクのコメントが投稿される（マーカー + ビルド URL 入り）。
    let comment = app
        .github()
        .wait_for_pr_comment(repo, 7, POLL_TIMEOUT)
        .await;
    assert_eq!(comment.method, wiremock::http::Method::POST, "新規作成");
    let comment_body: Value = serde_json::from_slice(&comment.body).expect("comment json");
    let text = comment_body["body"].as_str().expect("comment body text");
    assert!(
        text.contains(&format!("<!-- vrt:{} -->", project.project_id)),
        "マーカーを含む: {text}"
    );
    assert!(
        text.contains(&format!(
            "{}/t/{}/p/{}/builds/{number}",
            app.base_url(),
            project.tenant_slug,
            project.project_slug
        )),
        "ビルド URL を含む: {text}"
    );

    // 認証ヘッダの確認: installation access token が Bearer で付いている。
    let requests = app.github().status_requests(repo, &sha).await;
    assert!(!requests.is_empty(), "wiremock received status posts");
    for request in &requests {
        let auth = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            auth,
            format!("Bearer {TEST_INSTALLATION_TOKEN}"),
            "commit status must use the installation access token"
        );
    }

    // installation token の取得は App JWT（RS256, 3 セグメント）で行われている。
    let token_requests: Vec<_> = app
        .github()
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|req| req.url.path().ends_with("/access_tokens"))
        .collect();
    assert!(
        !token_requests.is_empty(),
        "installation access token was fetched"
    );
    let jwt = token_requests[0]
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .expect("app jwt bearer header");
    assert_eq!(
        jwt.split('.').count(),
        3,
        "app authentication uses a JWT, got {jwt}"
    );

    // トークンは Valkey にキャッシュされるので、ステータスを 2 回投げても取得は 1 回で済む。
    assert_eq!(
        token_requests.len(),
        1,
        "installation token should be cached in valkey"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejecting_a_build_posts_a_failure_status() {
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();
    let repo = "acme-inc/web";
    let sha = Uuid::new_v4().simple().to_string();

    app.github().expect_commit_statuses(repo, &sha).await;

    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;
    app.post_json(
        &format!("/v1/github/installations/{installation_id}/claim"),
        json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&app, &project.tenant_id).await }),
    )
    .await;
    app.patch_json(
        &format!("/v1/projects/{}/github", project.project_id),
        json!({ "installation_id": installation_id, "github_repo": repo }),
    )
    .await;

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;

    let res = app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                project.tenant_slug, project.project_slug
            ),
            &token,
            json!({ "branch": "feature/y", "commit_sha": sha }),
        )
        .await;
    let build: Value = res.json().await.expect("build json");
    let build_id: Uuid = build["id"].as_str().expect("build id").parse().unwrap();

    app.upload_screenshot(build_id, &token, "home", png(8, 8, [0, 255, 0, 255]))
        .await;
    app.post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &token)
        .await;

    let build = wait_for_terminal(&app, build_id, &token).await;
    assert_eq!(build["status"].as_str(), Some("changes_detected"));

    let res = app
        .post_json(&format!("/v1/builds/{build_id}/reject"), json!({}))
        .await;
    assert_eq!(res.status(), StatusCode::OK, "reject build");

    let failure = app
        .github()
        .wait_for_status(repo, &sha, "failure", POLL_TIMEOUT)
        .await;
    assert_eq!(
        failure["description"].as_str(),
        Some("Visual changes rejected")
    );

    // PR に紐付かないビルドはコメント API に一切触らない。
    let comment_requests: Vec<_> = app
        .github()
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|req| req.url.path().contains("/issues/"))
        .collect();
    assert!(
        comment_requests.is_empty(),
        "build without a PR must not touch the comments API"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_pr_comment_is_updated_instead_of_duplicated() {
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();
    let repo = "acme-inc/web";
    let sha = Uuid::new_v4().simple().to_string();

    app.github().expect_commit_statuses(repo, &sha).await;
    // マーカー入りの既存コメントがある → 更新（PATCH）経路。
    app.github()
        .expect_pr_comments(
            repo,
            9,
            json!([
                { "id": 11, "body": "just a human comment" },
                { "id": 55, "body": format!("<!-- vrt:{} -->\nold body", project.project_id) },
            ]),
        )
        .await;

    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;
    app.post_json(
        &format!("/v1/github/installations/{installation_id}/claim"),
        json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(&app, &project.tenant_id).await }),
    )
    .await;
    app.patch_json(
        &format!("/v1/projects/{}/github", project.project_id),
        json!({ "installation_id": installation_id, "github_repo": repo }),
    )
    .await;

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;

    let res = app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                project.tenant_slug, project.project_slug
            ),
            &token,
            json!({ "branch": "feature/z", "commit_sha": sha, "pull_request_number": 9 }),
        )
        .await;
    let build: Value = res.json().await.expect("build json");
    let build_id: Uuid = build["id"].as_str().expect("build id").parse().unwrap();

    app.upload_screenshot(build_id, &token, "home", png(8, 8, [0, 0, 255, 255]))
        .await;
    app.post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &token)
        .await;

    let comment = app
        .github()
        .wait_for_pr_comment(repo, 9, POLL_TIMEOUT)
        .await;
    assert_eq!(
        comment.method,
        wiremock::http::Method::PATCH,
        "既存のマーカーコメントは更新する（重複作成しない）"
    );
    assert!(
        comment.url.path().ends_with("/issues/comments/55"),
        "マーカーを含むコメント (id=55) を更新する: {}",
        comment.url.path()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_build_job_does_not_overwrite_newer_pr_comment() {
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();
    let repo = "acme-inc/web";
    let pr_number = 10;
    let old_sha = Uuid::new_v4().simple().to_string();
    let new_sha = Uuid::new_v4().simple().to_string();

    app.github().expect_commit_statuses(repo, &old_sha).await;
    app.github().expect_commit_statuses(repo, &new_sha).await;

    mount_stateful_pr_comment(&app, repo, pr_number).await;
    link_project_to_repo(&app, &project, installation_id, repo).await;

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;
    let old_build = create_pr_build(&app, &project, &token, &old_sha, pr_number).await;
    let new_build = create_pr_build(&app, &project, &token, &new_sha, pr_number).await;
    let old_build_id = old_build["id"].as_str().unwrap().parse().unwrap();
    let new_build_id = new_build["id"].as_str().unwrap().parse().unwrap();
    let old_number = old_build["number"].as_i64().unwrap();
    let new_number = new_build["number"].as_i64().unwrap();
    assert!(new_number > old_number);

    let job_state = backend::server::job_state_from(&app.state);
    job::github_status::process(
        job::GithubStatusJob {
            build_id: new_build_id,
        },
        Data::new(job_state.clone()),
    )
    .await
    .expect("new build status job");

    let new_comment = latest_comment_body(&app, repo, pr_number).await;
    assert!(new_comment.contains(&format!("<!-- vrt:build_number:{new_number} -->")));
    assert!(new_comment.contains(&format!("/builds/{new_number}")));
    assert_eq!(
        app.github()
            .pr_comment_requests(repo, i64::from(pr_number))
            .await
            .len(),
        1
    );

    // 新しいビルドの後で古いジョブを実行しても、コメントは書き戻されない。
    job::github_status::process(
        job::GithubStatusJob {
            build_id: old_build_id,
        },
        Data::new(job_state),
    )
    .await
    .expect("stale build status job");

    let final_comment = latest_comment_body(&app, repo, pr_number).await;
    assert_eq!(final_comment, new_comment);
    assert!(final_comment.contains(&format!("/builds/{new_number}")));
    assert!(!final_comment.contains(&format!("/builds/{old_number}")));
    assert_eq!(
        app.github()
            .pr_comment_requests(repo, i64::from(pr_number))
            .await
            .len(),
        1,
        "stale job must not POST or PATCH the PR comment"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn same_build_state_transition_still_updates_the_pr_comment() {
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();
    let repo = "acme-inc/web";
    let pr_number = 11;
    let sha = Uuid::new_v4().simple().to_string();

    app.github().expect_commit_statuses(repo, &sha).await;
    mount_stateful_pr_comment(&app, repo, pr_number).await;

    link_project_to_repo(&app, &project, installation_id, repo).await;

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;
    let build = create_pr_build(&app, &project, &token, &sha, pr_number).await;
    let build_id: Uuid = build["id"].as_str().unwrap().parse().unwrap();

    let job_state = backend::server::job_state_from(&app.state);
    job::github_status::process(
        job::GithubStatusJob { build_id },
        Data::new(job_state.clone()),
    )
    .await
    .expect("initial status job");

    let first = latest_comment_body(&app, repo, pr_number).await;
    assert!(
        first.contains("Waiting for screenshots"),
        "作成直後のビルドの description: {first}"
    );

    // 同じビルドが承認まで進む（finalize / approve は同じ build_id でジョブを投げ直す）。
    let row = entity::builds::Entity::find_by_id(build_id)
        .one(&app.state.db)
        .await
        .expect("load build")
        .expect("build row");
    let mut active: entity::builds::ActiveModel = row.into();
    active.status = Set(entity::builds::BuildStatus::Approved);
    active.update(&app.state.db).await.expect("approve build");

    job::github_status::process(job::GithubStatusJob { build_id }, Data::new(job_state))
        .await
        .expect("second status job for the same build");

    let second = latest_comment_body(&app, repo, pr_number).await;
    assert!(
        second.contains("Visual changes approved"),
        "同一ビルドの状態遷移はコメントに反映される: {second}"
    );
    assert_eq!(
        app.github()
            .pr_comment_requests(repo, i64::from(pr_number))
            .await
            .len(),
        2,
        "同一ビルドの 2 回目のジョブも書き込む（stale 判定で弾かない）"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_build_job_does_not_overwrite_newer_commit_status() {
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();
    let repo = "acme-inc/web";
    let pr_number = 12;
    // 同じコミットを 2 回ビルドした状況（再試行など）。commit status は SHA 単位なので
    // 古いビルドのジョブが新しいビルドの結果を上書きしうる。
    let sha = Uuid::new_v4().simple().to_string();

    app.github().expect_commit_statuses(repo, &sha).await;
    mount_stateful_pr_comment(&app, repo, pr_number).await;

    link_project_to_repo(&app, &project, installation_id, repo).await;

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;
    let old_build = create_pr_build(&app, &project, &token, &sha, pr_number).await;
    let new_build = create_pr_build(&app, &project, &token, &sha, pr_number).await;
    let old_build_id: Uuid = old_build["id"].as_str().unwrap().parse().unwrap();
    let new_build_id: Uuid = new_build["id"].as_str().unwrap().parse().unwrap();
    let old_number = old_build["number"].as_i64().unwrap();
    let new_number = new_build["number"].as_i64().unwrap();
    assert!(new_number > old_number);

    let job_state = backend::server::job_state_from(&app.state);
    job::github_status::process(
        job::GithubStatusJob {
            build_id: new_build_id,
        },
        Data::new(job_state.clone()),
    )
    .await
    .expect("new build status job");

    job::github_status::process(
        job::GithubStatusJob {
            build_id: old_build_id,
        },
        Data::new(job_state),
    )
    .await
    .expect("stale build status job");

    let statuses = app.github().status_requests(repo, &sha).await;
    assert_eq!(
        statuses.len(),
        1,
        "古いビルドのジョブは commit status を書き戻さない"
    );
    let body: Value = serde_json::from_slice(&statuses[0].body).expect("status json");
    assert_eq!(
        body["target_url"].as_str(),
        Some(
            format!(
                "{}/t/{}/p/{}/builds/{new_number}",
                app.base_url, project.tenant_slug, project.project_slug
            )
            .as_str()
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_status_guard_respects_project_boundary_in_status_history() {
    // commit status の context (`vrt`) は全プロジェクト共通で、ビルド番号はプロジェクトごとの
    // 独立した連番。同じ repo に紐づく別プロジェクトの番号と比べると、番号が小さいだけの
    // プロジェクトが恒久的にステータスを書けなくなる。
    let app = TestApp::new_with_github().await;
    let user = app.login_as_new_user().await;
    let leader = create_tenant_and_project(&app).await;
    let installation_id = unique_installation_id();
    let repo = "acme-inc/web";
    let pr_number = 13;
    let sha = Uuid::new_v4().simple().to_string();

    app.github().expect_commit_statuses(repo, &sha).await;
    mount_stateful_pr_comment(&app, repo, pr_number).await;
    link_project_to_repo(&app, &leader, installation_id, repo).await;

    // 同じテナント・同じリポジトリの 2 つ目のプロジェクト（例: docs と storybook）。
    let follower = create_sibling_project(&app, &leader).await;
    app.patch_json(
        &format!("/v1/projects/{}/github", follower.project_id),
        json!({ "installation_id": installation_id, "github_repo": repo }),
    )
    .await;

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;

    // leader 側の番号を進めてから status を書く。follower の 1 本目より大きい番号になる。
    let stale_leader_build = create_pr_build(&app, &leader, &token, &sha, pr_number).await;
    let leader_build = create_pr_build(&app, &leader, &token, &sha, pr_number).await;
    let follower_build = create_pr_build(&app, &follower, &token, &sha, pr_number).await;
    let leader_number = leader_build["number"].as_i64().unwrap();
    let follower_number = follower_build["number"].as_i64().unwrap();
    assert!(
        leader_number > follower_number,
        "別プロジェクトの方が大きい番号を持つ状況を作る"
    );

    let job_state = backend::server::job_state_from(&app.state);
    job::github_status::process(
        job::GithubStatusJob {
            build_id: leader_build["id"].as_str().unwrap().parse().unwrap(),
        },
        Data::new(job_state.clone()),
    )
    .await
    .expect("leader status job");

    job::github_status::process(
        job::GithubStatusJob {
            build_id: follower_build["id"].as_str().unwrap().parse().unwrap(),
        },
        Data::new(job_state.clone()),
    )
    .await
    .expect("follower status job");

    let statuses = app.github().status_requests(repo, &sha).await;
    assert_eq!(
        statuses.len(),
        2,
        "別プロジェクトのビルド番号で stale 判定してはいけない"
    );
    let body: Value = serde_json::from_slice(&statuses[1].body).expect("status json");
    assert_eq!(
        body["target_url"].as_str(),
        Some(
            format!(
                "{}/t/{}/p/{}/builds/{follower_number}",
                app.base_url, follower.tenant_slug, follower.project_slug
            )
            .as_str()
        )
    );

    // leader の古いジョブが遅れて届く。直近のステータスは follower のものなので、
    // 自プロジェクトの最新（leader_number）を履歴から探せないと巻き戻る。
    job::github_status::process(
        job::GithubStatusJob {
            build_id: stale_leader_build["id"].as_str().unwrap().parse().unwrap(),
        },
        Data::new(job_state),
    )
    .await
    .expect("stale leader status job");

    assert_eq!(
        app.github().status_requests(repo, &sha).await.len(),
        2,
        "別プロジェクトの status が間に挟まっても、古い自プロジェクトのビルドは弾く"
    );
}

/// 同じテナントにもう 1 つプロジェクトを作る。
async fn create_sibling_project(app: &TestApp, sibling: &Project) -> Project {
    let project_slug = format!("docs-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let res = app
        .post_json(
            &format!("/v1/tenants/{}/projects", sibling.tenant_id),
            json!({ "name": "Docs", "slug": project_slug }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED, "create sibling project");
    let project: Value = res.json().await.expect("project json");

    Project {
        tenant_id: sibling.tenant_id.clone(),
        tenant_slug: sibling.tenant_slug.clone(),
        project_id: project["id"].as_str().expect("project id").to_string(),
        project_slug,
    }
}

/// installation の webhook → claim → プロジェクト紐付けまでを一息でやる。
async fn link_project_to_repo(app: &TestApp, project: &Project, installation_id: i64, repo: &str) {
    app.post_github_webhook(
        "installation",
        &installation_payload("created", installation_id, "acme-inc"),
        None,
    )
    .await;
    app.wait_for_installation(installation_id, POLL_TIMEOUT)
        .await;
    app.post_json(
        &format!("/v1/github/installations/{installation_id}/claim"),
        json!({ "tenant_id": project.tenant_id, "state": issue_setup_state(app, &project.tenant_id).await }),
    )
    .await;
    app.patch_json(
        &format!("/v1/projects/{}/github", project.project_id),
        json!({ "installation_id": installation_id, "github_repo": repo }),
    )
    .await;
}

/// POST / PATCH した本文を保持し、以降の GET で返す PR コメント mock。
async fn mount_stateful_pr_comment(app: &TestApp, repo: &str, pr_number: i32) {
    let current = Arc::new(Mutex::new(None::<String>));

    let for_get = Arc::clone(&current);
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repo}/issues/{pr_number}/comments")))
        .respond_with(move |_request: &wiremock::Request| {
            let comments = for_get
                .lock()
                .expect("comment state lock")
                .as_ref()
                .map(|body| json!([{ "id": 100, "body": body }]))
                .unwrap_or_else(|| json!([]));
            ResponseTemplate::new(200).set_body_json(comments)
        })
        .mount(&app.github().server)
        .await;

    let for_post = Arc::clone(&current);
    Mock::given(method("POST"))
        .and(path(format!("/repos/{repo}/issues/{pr_number}/comments")))
        .respond_with(move |request: &wiremock::Request| {
            let payload: Value = serde_json::from_slice(&request.body).expect("comment json");
            *for_post.lock().expect("comment state lock") =
                Some(payload["body"].as_str().expect("comment body").to_owned());
            ResponseTemplate::new(201).set_body_json(json!({ "id": 100 }))
        })
        .mount(&app.github().server)
        .await;

    Mock::given(method("PATCH"))
        .and(path(format!("/repos/{repo}/issues/comments/100")))
        .respond_with(move |request: &wiremock::Request| {
            let payload: Value = serde_json::from_slice(&request.body).expect("comment json");
            *current.lock().expect("comment state lock") =
                Some(payload["body"].as_str().expect("comment body").to_owned());
            ResponseTemplate::new(200).set_body_json(json!({ "id": 100 }))
        })
        .mount(&app.github().server)
        .await;
}

/// 直近に書き込まれた PR コメントの本文（[`mount_stateful_pr_comment`] と対で使う）。
async fn latest_comment_body(app: &TestApp, repo: &str, pr_number: i32) -> String {
    let request = app
        .github()
        .pr_comment_requests(repo, i64::from(pr_number))
        .await
        .pop()
        .expect("a pr comment was written");
    let payload: Value = serde_json::from_slice(&request.body).expect("comment json");
    payload["body"].as_str().expect("comment body").to_string()
}

// ── 5. GitHub App 未設定 ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn build_flow_completes_without_github_app_configured() {
    // `TestApp::new()` は GitHub App を設定しない（wiremock も立てない）。
    // ステータスジョブは投入されるが、何もせずに Ok で終わるはず。
    let app = TestApp::new().await;
    let user = app.login_as_new_user().await;
    let project = create_tenant_and_project(&app).await;

    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;

    let res = app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                project.tenant_slug, project.project_slug
            ),
            &token,
            json!({ "branch": "main", "commit_sha": Uuid::new_v4().simple().to_string() }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let build: Value = res.json().await.expect("build json");
    let build_id: Uuid = build["id"].as_str().expect("build id").parse().unwrap();

    app.upload_screenshot(build_id, &token, "home", png(8, 8, [0, 0, 255, 255]))
        .await;
    let status = app
        .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), &token)
        .await
        .status();
    assert_eq!(status, StatusCode::OK, "finalize works without github app");

    let build = wait_for_terminal(&app, build_id, &token).await;
    assert_eq!(
        build["status"].as_str(),
        Some("changes_detected"),
        "compare job still runs to completion"
    );

    // 承認まで通ることも確認する（approve も status ジョブを投げる経路）。
    let res = app
        .post_json(
            &format!("/v1/builds/{build_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "approve works without github app"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_is_rejected_when_github_app_is_not_configured() {
    let app = TestApp::new().await;
    let res = app
        .post_github_webhook(
            "installation",
            &installation_payload("created", unique_installation_id(), "acme-inc"),
            None,
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "webhook endpoint reports that the app is not configured"
    );
    let body: Value = res.json().await.expect("error json");
    assert_eq!(body["message"].as_str(), Some("github app not configured"));
}

// ── 共通ヘルパー ────────────────────────────────────────────────────────

/// CI 用のポーリングエンドポイントで終端状態になるまで待つ。
async fn wait_for_terminal(app: &TestApp, build_id: Uuid, token: &str) -> Value {
    let deadline = std::time::Instant::now() + POLL_TIMEOUT;
    loop {
        let res = app
            .get_with_bearer(&format!("/v1/ci/builds/{build_id}"), token)
            .await;
        assert_eq!(res.status(), StatusCode::OK, "poll build status");
        let build: Value = res.json().await.expect("build json");
        let status = build["status"].as_str().unwrap_or_default().to_string();
        if !matches!(status.as_str(), "pending" | "queued" | "processing") {
            return build;
        }
        if std::time::Instant::now() >= deadline {
            panic!("build {build_id} stuck in {status} after {POLL_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ── 6. merge をまたいだ baseline の引き継ぎ ──────────────────────────────
//
// PR の枝で承認した baseline は、merge 後の default branch のビルドへ引き継がれる。
// default branch のビルドを作ると、`GET /repos/{repo}/commits/{sha}/pulls` で
// その commit を入れた merge 済みの PR を引き、PR の枝の最新 baseline を
// default branch の baseline として写す。

/// `GET /repos/{repo}/commits/{sha}/pulls` の 1 件分（実 API の形の抜粋）。
///
/// merge commit・squash・rebase のどれでも、default branch 上の commit から
/// merge 済みの PR が同じ形で 1 件返る（実 API で確かめた）。違うのは
/// `merge_commit_sha` がビルドした commit と一致するかどうかだけで、rebase の
/// 前の方の写しでは一致しない。
fn associated_pull(
    number: i64,
    head_ref: &str,
    base_ref: &str,
    merge_commit_sha: &str,
    merged: bool,
) -> Value {
    json!({
        "number": number,
        "state": if merged { "closed" } else { "open" },
        "merged_at": if merged { json!("2026-09-30T00:00:00Z") } else { Value::Null },
        "merge_commit_sha": merge_commit_sha,
        "head": { "ref": head_ref, "sha": Uuid::new_v4().simple().to_string() },
        "base": { "ref": base_ref },
    })
}

async fn mount_commit_pulls(app: &TestApp, repo: &str, sha: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repo}/commits/{sha}/pulls")))
        .respond_with(response)
        .mount(&app.github().server)
        .await;
}

/// CI ビルドを 1 本流し、作成時の応答と終端状態の応答を返す。
async fn run_ci_build(
    app: &TestApp,
    project: &Project,
    token: &str,
    branch: &str,
    sha: &str,
    pull_request_number: Option<i32>,
    color: [u8; 4],
) -> (Value, Value) {
    let res = app
        .post_json_with_bearer(
            &format!(
                "/v1/ci/projects/{}/{}/builds",
                project.tenant_slug, project.project_slug
            ),
            token,
            json!({
                "branch": branch,
                "commit_sha": sha,
                "pull_request_number": pull_request_number,
            }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CREATED,
        "create build on {branch}"
    );
    let created: Value = res.json().await.expect("build json");
    let build_id: Uuid = created["id"].as_str().expect("build id").parse().unwrap();

    let status = app
        .upload_screenshot(build_id, token, "home", png(8, 8, color))
        .await
        .status();
    assert_eq!(status, StatusCode::CREATED, "upload screenshot");
    let status = app
        .post_with_bearer(&format!("/v1/ci/builds/{build_id}/finalize"), token)
        .await
        .status();
    assert_eq!(status, StatusCode::OK, "finalize");

    let terminal = wait_for_terminal(app, build_id, token).await;
    (created, terminal)
}

async fn approve(app: &TestApp, build: &Value) {
    let build_id = build["id"].as_str().expect("build id");
    let res = app
        .post_json(
            &format!("/v1/builds/{build_id}/approve"),
            json!({ "force": true }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK, "approve build {build_id}");
}

async fn ci_token(app: &TestApp) -> String {
    let user = app.login_as_new_user().await;
    let (token, _) = app
        .insert_personal_token(
            user.id,
            vec![Scope::WriteBuild, Scope::ReadBuild, Scope::ReadProject],
        )
        .await;
    token
}

/// GitHub に紐付いたプロジェクトで、PR の枝のビルドを承認して baseline を作る。
async fn linked_project_with_pr_baseline(
    app: &TestApp,
    repo: &str,
    head_ref: &str,
    pr_sha: &str,
    pr_number: i32,
) -> (Project, String) {
    let token = ci_token(app).await;
    let project = create_tenant_and_project(app).await;
    link_project_to_repo(app, &project, unique_installation_id(), repo).await;
    app.github().expect_commit_statuses(repo, pr_sha).await;
    app.github()
        .expect_pr_comments(repo, i64::from(pr_number), json!([]))
        .await;

    let (_, pr_build) = run_ci_build(
        app,
        &project,
        &token,
        head_ref,
        pr_sha,
        Some(pr_number),
        [255, 0, 0, 255],
    )
    .await;
    assert_eq!(
        pr_build["status"].as_str(),
        Some("changes_detected"),
        "first build on the PR branch has nothing to compare against"
    );
    approve(app, &pr_build).await;
    (project, token)
}

/// merge 後の default branch のビルドが、PR の枝の baseline を引き継いだことを確かめる。
///
/// PR と同じ画像を撮るので、引き継いでいれば差分は出ない（passed）。
/// 引き継いでいなければ default branch に baseline が無く、全部 added になる。
async fn assert_main_build_inherits(
    app: &TestApp,
    project: &Project,
    token: &str,
    repo: &str,
    main_sha: &str,
    pr_sha: &str,
) {
    app.github().expect_commit_statuses(repo, main_sha).await;
    let (created, main_build) = run_ci_build(
        app,
        project,
        token,
        "main",
        main_sha,
        None,
        [255, 0, 0, 255],
    )
    .await;
    assert_eq!(
        created["baseline_commit_sha"].as_str(),
        Some(pr_sha),
        "the default-branch build starts from the PR's approved baseline"
    );
    assert_eq!(
        main_build["status"].as_str(),
        Some("passed"),
        "the PR's approval carries over the merge: {main_build}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_commit_inherits_the_pr_baseline() {
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-merge";
    let pr_sha = Uuid::new_v4().simple().to_string();
    let main_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/merge", &pr_sha, 21).await;

    // merge commit: default branch 上の merge commit そのものが merge_commit_sha。
    mount_commit_pulls(
        &app,
        repo,
        &main_sha,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            21,
            "feature/merge",
            "main",
            &main_sha,
            true
        )])),
    )
    .await;

    assert_main_build_inherits(&app, &project, &token, repo, &main_sha, &pr_sha).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn squash_merge_inherits_the_pr_baseline() {
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-squash";
    let pr_sha = Uuid::new_v4().simple().to_string();
    let main_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/squash", &pr_sha, 22).await;

    // squash: PR の commit は default branch に入らず、畳んだ 1 commit が merge_commit_sha。
    mount_commit_pulls(
        &app,
        repo,
        &main_sha,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            22,
            "feature/squash",
            "main",
            &main_sha,
            true
        )])),
    )
    .await;

    assert_main_build_inherits(&app, &project, &token, repo, &main_sha, &pr_sha).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rebase_merge_inherits_the_pr_baseline() {
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-rebase";
    let pr_sha = Uuid::new_v4().simple().to_string();
    let first_copy = Uuid::new_v4().simple().to_string();
    let last_copy = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/rebase", &pr_sha, 23).await;

    // rebase: PR の commit が写しとして default branch に並び、merge_commit_sha は
    // 最後の写しを指す。前の方の写しでビルドしても同じ PR が返る。
    mount_commit_pulls(
        &app,
        repo,
        &first_copy,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            23,
            "feature/rebase",
            "main",
            &last_copy,
            true
        )])),
    )
    .await;

    assert_main_build_inherits(&app, &project, &token, repo, &first_copy, &pr_sha).await;
}

/// 引き継がないときの default branch のビルド（現行の二段解決のまま）。
async fn assert_main_build_does_not_inherit(
    app: &TestApp,
    project: &Project,
    token: &str,
    main_sha: &str,
) {
    let (created, main_build) = run_ci_build(
        app,
        project,
        token,
        "main",
        main_sha,
        None,
        [255, 0, 0, 255],
    )
    .await;
    assert!(
        created["baseline_commit_sha"].is_null(),
        "default branch has no baseline of its own: {created}"
    );
    assert_eq!(
        main_build["status"].as_str(),
        Some("changes_detected"),
        "without inheritance every screenshot is new on the default branch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_github_app_the_default_branch_keeps_its_own_resolution() {
    // GitHub App 未設定（`TestApp::new`）。PR の枝で承認しても、default branch の
    // ビルドはいまの二段解決（同じ枝 → default branch）のまま引き継がない。
    let app = TestApp::new().await;
    let token = ci_token(&app).await;
    let project = create_tenant_and_project(&app).await;
    let pr_sha = Uuid::new_v4().simple().to_string();

    let (_, pr_build) = run_ci_build(
        &app,
        &project,
        &token,
        "feature/no-app",
        &pr_sha,
        Some(24),
        [255, 0, 0, 255],
    )
    .await;
    approve(&app, &pr_build).await;

    assert_main_build_does_not_inherit(
        &app,
        &project,
        &token,
        &Uuid::new_v4().simple().to_string(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_pull_lookup_falls_back_without_failing_the_build() {
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-api-down";
    let pr_sha = Uuid::new_v4().simple().to_string();
    let main_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/api-down", &pr_sha, 25).await;
    app.github().expect_commit_statuses(repo, &main_sha).await;

    mount_commit_pulls(&app, repo, &main_sha, ResponseTemplate::new(500)).await;

    assert_main_build_does_not_inherit(&app, &project, &token, &main_sha).await;
    let asked = app
        .github()
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .any(|req| req.url.path() == format!("/repos/{repo}/commits/{main_sha}/pulls"));
    assert!(asked, "the lookup was attempted before falling back");
}

#[tokio::test(flavor = "multi_thread")]
async fn unmerged_pull_is_not_inherited() {
    // default branch の commit が open の PR にも載っている（例: PR の枝を default から
    // 切った直後）。merge されていない PR の baseline は引き継がない。
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-open";
    let pr_sha = Uuid::new_v4().simple().to_string();
    let main_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/open", &pr_sha, 26).await;
    app.github().expect_commit_statuses(repo, &main_sha).await;

    mount_commit_pulls(
        &app,
        repo,
        &main_sha,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            26,
            "feature/open",
            "main",
            &main_sha,
            false
        )])),
    )
    .await;

    assert_main_build_does_not_inherit(&app, &project, &token, &main_sha).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn newer_default_branch_baseline_is_not_overwritten() {
    // PR の承認より後に default branch で承認されていれば、そちらが新しい。
    // 古い PR の baseline で default branch を巻き戻さない。
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-stale";
    let pr_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/stale", &pr_sha, 27).await;

    // default branch で別の画像を承認する（この commit には PR が紐付かない）。
    let main_sha_1 = Uuid::new_v4().simple().to_string();
    app.github().expect_commit_statuses(repo, &main_sha_1).await;
    mount_commit_pulls(
        &app,
        repo,
        &main_sha_1,
        ResponseTemplate::new(200).set_body_json(json!([])),
    )
    .await;
    let (_, main_build_1) = run_ci_build(
        &app,
        &project,
        &token,
        "main",
        &main_sha_1,
        None,
        [0, 0, 255, 255],
    )
    .await;
    approve(&app, &main_build_1).await;

    // その後に PR の merge が default branch へ入る。
    let main_sha_2 = Uuid::new_v4().simple().to_string();
    app.github().expect_commit_statuses(repo, &main_sha_2).await;
    mount_commit_pulls(
        &app,
        repo,
        &main_sha_2,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            27,
            "feature/stale",
            "main",
            &main_sha_2,
            true
        )])),
    )
    .await;
    let (created, _) = run_ci_build(
        &app,
        &project,
        &token,
        "main",
        &main_sha_2,
        None,
        [255, 0, 0, 255],
    )
    .await;
    assert_eq!(
        created["baseline_commit_sha"].as_str(),
        Some(main_sha_1.as_str()),
        "the newer default-branch baseline stays in place"
    );
}

/// プロジェクトの default branch（main）に在る baseline の行の数。
async fn main_baseline_rows(app: &TestApp, project: &Project) -> u64 {
    use sea_orm::{ColumnTrait, PaginatorTrait, QueryFilter};

    let project_id: Uuid = project.project_id.parse().expect("project id");
    entity::baselines::Entity::find()
        .filter(entity::baselines::Column::ProjectId.eq(project_id))
        .filter(entity::baselines::Column::Branch.eq("main"))
        .count(&app.state.db)
        .await
        .expect("count baselines")
}

/// PR の枝のビルドを流して承認する（同じプロジェクトに二本目以降の PR を足す）。
#[allow(clippy::too_many_arguments)]
async fn approve_pr_build(
    app: &TestApp,
    project: &Project,
    token: &str,
    repo: &str,
    head_ref: &str,
    sha: &str,
    pr_number: i32,
    color: [u8; 4],
) {
    app.github().expect_commit_statuses(repo, sha).await;
    app.github()
        .expect_pr_comments(repo, i64::from(pr_number), json!([]))
        .await;
    let (_, build) = run_ci_build(app, project, token, head_ref, sha, Some(pr_number), color).await;
    approve(app, &build).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reused_branch_name_does_not_inherit_another_pull_requests_baseline() {
    // merge 済みの PR #31 の枝名を、あとで未 merge の PR #32 が使い回し、その baseline が
    // 承認された。#31 の merge commit のビルドを（再実行で）作っても、写るのは #31 の
    // baseline であって、枝名が同じだけの #32 の baseline ではない。
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-reused";
    let pr31_sha = Uuid::new_v4().simple().to_string();
    let pr32_sha = Uuid::new_v4().simple().to_string();
    let main_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/reused", &pr31_sha, 31).await;

    // 同じ枝名で別の PR #32 を開き、違う絵を承認する（#31 より新しい baseline になる）。
    approve_pr_build(
        &app,
        &project,
        &token,
        repo,
        "feature/reused",
        &pr32_sha,
        32,
        [0, 0, 255, 255],
    )
    .await;

    mount_commit_pulls(
        &app,
        repo,
        &main_sha,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            31,
            "feature/reused",
            "main",
            &main_sha,
            true
        )])),
    )
    .await;

    // #31 と同じ絵（赤）を撮る。写ったのが #31 なら差分は出ない。
    assert_main_build_inherits(&app, &project, &token, repo, &main_sha, &pr31_sha).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn second_build_of_the_same_merge_commit_does_not_inherit_again() {
    // 同じ merge commit で main のビルドを二度作る（CI の再実行など）。二度目は写さない。
    // baseline_commit_sha が同じであることでは足りない（created_at の守りでも同じになる）
    // ので、default branch の baseline の行が増えないことを直に数える。
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-twice";
    let pr_sha = Uuid::new_v4().simple().to_string();
    let main_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/twice", &pr_sha, 33).await;
    mount_commit_pulls(
        &app,
        repo,
        &main_sha,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            33,
            "feature/twice",
            "main",
            &main_sha,
            true
        )])),
    )
    .await;

    assert_eq!(main_baseline_rows(&app, &project).await, 0);
    assert_main_build_inherits(&app, &project, &token, repo, &main_sha, &pr_sha).await;
    assert_eq!(
        main_baseline_rows(&app, &project).await,
        1,
        "the first build of the merge commit inherits once"
    );

    let (created, _) = run_ci_build(
        &app,
        &project,
        &token,
        "main",
        &main_sha,
        None,
        [255, 0, 0, 255],
    )
    .await;
    assert_eq!(
        created["baseline_commit_sha"].as_str(),
        Some(pr_sha.as_str())
    );
    assert_eq!(
        main_baseline_rows(&app, &project).await,
        1,
        "the second build of the same merge commit does not copy the baseline again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_request_merged_after_an_inheritance_is_not_inherited() {
    // 境界を意図として固定する。PR A と PR B をこの順に承認し、A を merge（写る）、
    // そのあと B を merge する。B の baseline は、A を写した行（写した時刻が入る）より
    // 古いので引き継がない。名前ごとに合成する本筋は VRT-26 で扱う。
    let app = TestApp::new_with_github().await;
    let repo = "acme-inc/inherit-boundary";
    let a_sha = Uuid::new_v4().simple().to_string();
    let b_sha = Uuid::new_v4().simple().to_string();
    let (project, token) =
        linked_project_with_pr_baseline(&app, repo, "feature/a", &a_sha, 34).await;
    approve_pr_build(
        &app,
        &project,
        &token,
        repo,
        "feature/b",
        &b_sha,
        35,
        [0, 0, 255, 255],
    )
    .await;

    // A を merge する。A の baseline が写る。
    let main_a = Uuid::new_v4().simple().to_string();
    mount_commit_pulls(
        &app,
        repo,
        &main_a,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            34,
            "feature/a",
            "main",
            &main_a,
            true
        )])),
    )
    .await;
    assert_main_build_inherits(&app, &project, &token, repo, &main_a, &a_sha).await;

    // B を merge する。B の baseline は写した行より古いので、main は A のまま。
    let main_b = Uuid::new_v4().simple().to_string();
    app.github().expect_commit_statuses(repo, &main_b).await;
    mount_commit_pulls(
        &app,
        repo,
        &main_b,
        ResponseTemplate::new(200).set_body_json(json!([associated_pull(
            35,
            "feature/b",
            "main",
            &main_b,
            true
        )])),
    )
    .await;
    let (created, _) = run_ci_build(
        &app,
        &project,
        &token,
        "main",
        &main_b,
        None,
        [0, 0, 255, 255],
    )
    .await;
    assert_eq!(
        created["baseline_commit_sha"].as_str(),
        Some(a_sha.as_str()),
        "a pull request merged after an inheritance is not inherited (VRT-26)"
    );
    assert_eq!(main_baseline_rows(&app, &project).await, 1);
}
