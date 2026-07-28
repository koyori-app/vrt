use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(
            crate::handlers::tenants::list_tenants,
            crate::handlers::tenants::create_tenant
        ))
        .routes(routes!(
            crate::handlers::tenants::get_tenant,
            crate::handlers::tenants::update_tenant,
            crate::handlers::tenants::delete_tenant
        ))
        .nest(
            "/{tenant_id}/members",
            OpenApiRouter::<AppState>::new()
                .routes(routes!(
                    crate::handlers::tenants::list_members,
                    crate::handlers::tenants::add_member
                ))
                .routes(routes!(
                    crate::handlers::tenants::update_member,
                    crate::handlers::tenants::remove_member
                )),
        )
        .nest(
            "/{tenant_id}/projects",
            OpenApiRouter::<AppState>::new().routes(routes!(
                crate::handlers::projects::list_projects,
                crate::handlers::projects::create_project
            )),
        )
}
