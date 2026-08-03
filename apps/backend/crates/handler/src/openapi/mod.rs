//! OpenAPI コンポーネント登録。

pub mod responses;

use utoipa::openapi::OpenApi;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::tag::TagBuilder;
use utoipa::{PartialSchema, ToSchema};

pub use crate::error::ServerError;
pub use responses::{
    CrudErrors, InternalOnlyError, OAuthErrors, SessionAuthErrors, UnauthorizedErrors,
};

/// スキーマのうち、ハンドラだけでは OpenAPI に載らないものを登録する。
pub fn register_schemas(openapi: &mut OpenApi) {
    let components = openapi
        .components
        .get_or_insert_with(utoipa::openapi::Components::new);

    register_schema::<ServerError>(components);
    register_schema::<entity::scopes::Scope>(components);
    register_schema::<entity::scopes::ScopeList>(components);
    register_schema::<payload::users::MeResponse>(components);
    register_schema::<payload::personal_tokens::PersonalTokenResponse>(components);
    register_schema::<payload::personal_tokens::CreatePersonalTokenRequest>(components);
    register_schema::<payload::personal_tokens::CreatePersonalTokenResponse>(components);
    register_schema::<entity::tenant_members::TenantRole>(components);
    register_schema::<payload::tenants::TenantResponse>(components);
    register_schema::<payload::tenants::CreateTenantRequest>(components);
    register_schema::<payload::tenants::UpdateTenantRequest>(components);
    register_schema::<payload::tenants::TenantMemberResponse>(components);
    register_schema::<payload::tenants::AddMemberRequest>(components);
    register_schema::<payload::tenants::UpdateMemberRequest>(components);
    register_schema::<payload::projects::ProjectResponse>(components);
    register_schema::<payload::projects::CreateProjectRequest>(components);
    register_schema::<payload::projects::UpdateProjectRequest>(components);
    register_schema::<entity::builds::BuildStatus>(components);
    register_schema::<entity::comparisons::ComparisonStatus>(components);
    register_schema::<entity::comparisons::ReviewStatus>(components);
    register_schema::<payload::builds::BuildResponse>(components);
    register_schema::<payload::builds::BuildListResponse>(components);
    register_schema::<payload::builds::CreateBuildRequest>(components);
    register_schema::<payload::builds::FinalizeBuildRequest>(components);
    register_schema::<payload::builds::ScreenshotResponse>(components);
    register_schema::<payload::builds::ApproveBuildRequest>(components);
    register_schema::<payload::builds::BuildLogEntry>(components);
    register_schema::<payload::builds::BuildLogsResponse>(components);
    register_schema::<payload::comparisons::ComparisonResponse>(components);
    register_schema::<payload::comparisons::ComparisonListResponse>(components);
    register_schema::<payload::comparisons::ReviewActionRequest>(components);
    register_schema::<payload::comparisons::ReviewComparisonRequest>(components);
    register_schema::<payload::github::GithubInstallationResponse>(components);
    register_schema::<payload::github::GithubInstallationListResponse>(components);
    register_schema::<payload::github::GithubAppResponse>(components);
    register_schema::<payload::github::GithubRepositoryResponse>(components);
    register_schema::<payload::github::GithubRepositoryListResponse>(components);
    register_schema::<payload::github::ClaimInstallationRequest>(components);
    register_schema::<payload::github::UpdateProjectGithubRequest>(components);
    register_security_schemes(components);
    register_tags(openapi);
}

/// Scalar のグループ表示順とタグ説明を定義する。
fn register_tags(openapi: &mut OpenApi) {
    openapi.tags = Some(vec![
        TagBuilder::new()
            .name("Health")
            .description(Some("死活監視"))
            .build(),
        TagBuilder::new()
            .name("Auth")
            .description(Some("OAuth ログイン / ログアウト"))
            .build(),
        TagBuilder::new()
            .name("Users")
            .description(Some("ユーザー"))
            .build(),
        TagBuilder::new()
            .name("Personal Tokens")
            .description(Some("パーソナルアクセストークン (PAT)"))
            .build(),
        TagBuilder::new()
            .name("Tenants")
            .description(Some("テナント（組織）"))
            .build(),
        TagBuilder::new()
            .name("Tenant Members")
            .description(Some("テナントメンバーとロール"))
            .build(),
        TagBuilder::new()
            .name("Projects")
            .description(Some("VRT プロジェクト"))
            .build(),
        TagBuilder::new()
            .name("Builds")
            .description(Some("ビルドとレビュー"))
            .build(),
        TagBuilder::new()
            .name("Comparisons")
            .description(Some("比較結果と差分画像"))
            .build(),
        TagBuilder::new()
            .name("Screenshots")
            .description(Some("スクリーンショット画像"))
            .build(),
        TagBuilder::new()
            .name("Baselines")
            .description(Some("baseline 画像"))
            .build(),
        TagBuilder::new()
            .name("CI")
            .description(Some("CI クライアント向け"))
            .build(),
        TagBuilder::new()
            .name("GitHub")
            .description(Some(
                "GitHub App 連携（installation / webhook / PR ステータス）",
            ))
            .build(),
    ]);
}

/// PAT 等で利用する Bearer 認証スキーム（`Authorization: Bearer <token>`）。
fn register_security_schemes(components: &mut utoipa::openapi::Components) {
    components.security_schemes.insert(
        "bearerAuth".to_string(),
        SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .bearer_format("PAT")
                .build(),
        ),
    );
}

fn register_schema<T>(components: &mut utoipa::openapi::Components)
where
    T: ToSchema + PartialSchema,
{
    let name = T::name().into_owned();
    components.schemas.entry(name).or_insert_with(T::schema);
}
