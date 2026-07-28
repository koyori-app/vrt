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

use std::time::Duration;

use common::{TEST_INSTALLATION_TOKEN, TestApp};
use entity::scopes::Scope;
use image::{Rgba, RgbaImage};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

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
            json!({ "tenant_id": project.tenant_id }),
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
            json!({ "tenant_id": project.tenant_id }),
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
            json!({ "tenant_id": project.tenant_id }),
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
            json!({ "tenant_id": project.tenant_id }),
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
            json!({ "tenant_id": other.tenant_id }),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "installation already claimed by another tenant"
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
        json!({ "tenant_id": project.tenant_id }),
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
            json!({ "tenant_id": project.tenant_id }),
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

    // finalize 直後は pending（processing）。
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
        json!({ "tenant_id": project.tenant_id }),
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
        if !matches!(status.as_str(), "pending" | "processing") {
            return build;
        }
        if std::time::Instant::now() >= deadline {
            panic!("build {build_id} stuck in {status} after {POLL_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
