use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

/// ビルド ID 直参照のルート（プロジェクト配下の一覧は `routes::projects` 側）。
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(crate::handlers::builds::get_build))
        .routes(routes!(crate::handlers::builds::get_build_logs))
        .routes(routes!(crate::handlers::builds::list_comparisons))
        .routes(routes!(crate::handlers::builds::approve_build))
        .routes(routes!(crate::handlers::builds::reject_build))
        // Open Storybook: アップロード済みバンドルの静的配信。`{*path}` は axum の
        // キャッチオールなので `assets/foo.js` のようなネストしたパスも 1 ルートで拾える。
        .routes(routes!(crate::handlers::content::get_storybook_index))
        .routes(routes!(crate::handlers::content::get_storybook_asset))
}
