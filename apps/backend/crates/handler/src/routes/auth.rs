use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(crate::handlers::auth::oauth_login))
        .routes(routes!(crate::handlers::auth::oauth_callback))
        .routes(routes!(crate::handlers::auth::logout))
        // TEST_LOGIN_ENABLED=true のときだけ機能する。既定では 404 を返す。
        .routes(routes!(crate::handlers::auth::test_login))
}
