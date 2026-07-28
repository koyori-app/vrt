use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(
            crate::handlers::personal_tokens::list_personal_tokens,
            crate::handlers::personal_tokens::create_personal_token
        ))
        .routes(routes!(
            crate::handlers::personal_tokens::revoke_personal_token
        ))
}
