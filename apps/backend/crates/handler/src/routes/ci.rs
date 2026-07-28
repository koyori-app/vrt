use tower_http::limit::RequestBodyLimitLayer;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use service::screenshots::MAX_UPLOAD_BYTES;

/// multipart のオーバーヘッド（境界・ヘッダ）ぶんの余裕。
const MULTIPART_OVERHEAD: usize = 64 * 1024;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(crate::handlers::ci::ping))
        .routes(routes!(crate::handlers::ci::create_build))
        .routes(routes!(crate::handlers::ci::get_build_status))
        .routes(routes!(crate::handlers::ci::finalize_build))
        .routes(routes!(crate::handlers::ci::upload_screenshot))
        // axum の既定ボディ上限は 2MB。スクリーンショットは 25MB まで許可する。
        .layer(RequestBodyLimitLayer::new(
            MAX_UPLOAD_BYTES + MULTIPART_OVERHEAD,
        ))
}
