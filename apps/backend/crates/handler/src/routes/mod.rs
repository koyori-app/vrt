use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

pub mod auth;
pub mod baseline_entries;
pub mod builds;
pub mod ci;
pub mod comparisons;
pub mod github;
pub mod personal_tokens;
pub mod projects;
pub mod screenshots;
pub mod tenants;
pub mod users;

pub fn create_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().nest(
        "/v1",
        OpenApiRouter::<AppState>::new()
            .routes(routes!(crate::handlers::health::health))
            .routes(routes!(crate::handlers::health::queues_health))
            .nest("/auth", auth::routes())
            .nest("/baseline-entries", baseline_entries::routes())
            .nest("/builds", builds::routes())
            .nest("/ci", ci::routes())
            .nest("/comparisons", comparisons::routes())
            .nest("/github", github::routes())
            .nest("/screenshots", screenshots::routes())
            .nest("/personal_tokens", personal_tokens::routes())
            .nest("/projects", projects::routes())
            .nest("/tenants", tenants::routes())
            .nest("/users", users::routes()),
    )
}
