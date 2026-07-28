use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

/// プロジェクト ID 直参照のルート（テナント配下の一覧・作成は `routes::tenants` 側）。
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(
            crate::handlers::projects::get_project,
            crate::handlers::projects::update_project,
            crate::handlers::projects::delete_project
        ))
        .routes(routes!(crate::handlers::builds::list_builds))
        .routes(routes!(crate::handlers::builds::get_build_by_number))
        .routes(routes!(crate::handlers::github::update_project_github))
}
