use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

/// ビルド ID 直参照のルート（プロジェクト配下の一覧は `routes::projects` 側）。
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(crate::handlers::builds::get_build))
        .routes(routes!(crate::handlers::builds::list_comparisons))
        .routes(routes!(crate::handlers::builds::approve_build))
        .routes(routes!(crate::handlers::builds::reject_build))
}
