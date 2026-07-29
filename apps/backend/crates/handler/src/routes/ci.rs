use axum::extract::DefaultBodyLimit;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use service::render::MAX_BUNDLE_BYTES;
use service::screenshots::MAX_UPLOAD_BYTES;

/// multipart のオーバーヘッド（境界・ヘッダ）ぶんの余裕。
const MULTIPART_OVERHEAD: usize = 64 * 1024;

pub fn routes() -> OpenApiRouter<AppState> {
    // ボディ上限はエンドポイントごとに違うのでレイヤーを分けて合成する。
    // 1 本のルーターに 200MB を張ると、スクリーンショットの 25MB 制限まで緩んでしまう。
    //
    // 上限は 2 段構えにする。
    // - `DefaultBodyLimit`: `Multipart` 抽出器が読み込むバイト数の上限。
    //   axum の既定は 2MB で、これを上げないと 2MB 超のアップロードが
    //   「invalid file field: Error parsing multipart/form-data request」で 400 になる。
    // - `RequestBodyLimitLayer`: 抽出器に届く前に切る外側の防御ライン
    //   （Content-Length ベースの早期拒否を含む）。冗長だが多層防御として残す。
    // 2 つは必ず同じ値にすること。片方だけ上げても、低いほうが実効上限になる。
    let screenshots = OpenApiRouter::<AppState>::new()
        .routes(routes!(crate::handlers::ci::ping))
        .routes(routes!(crate::handlers::ci::create_build))
        .routes(routes!(crate::handlers::ci::get_build_status))
        .routes(routes!(crate::handlers::ci::get_build_logs))
        .routes(routes!(crate::handlers::ci::finalize_build))
        .routes(routes!(crate::handlers::ci::upload_screenshot))
        // axum の既定ボディ上限は 2MB。スクリーンショットは 25MB まで許可する。
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + MULTIPART_OVERHEAD))
        .layer(RequestBodyLimitLayer::new(
            MAX_UPLOAD_BYTES + MULTIPART_OVERHEAD,
        ));

    let storybook = OpenApiRouter::<AppState>::new()
        .routes(routes!(crate::handlers::ci::upload_storybook_bundle))
        // storybook-static の zip は桁が違う（200MB）。
        .layer(DefaultBodyLimit::max(MAX_BUNDLE_BYTES + MULTIPART_OVERHEAD))
        .layer(RequestBodyLimitLayer::new(
            MAX_BUNDLE_BYTES + MULTIPART_OVERHEAD,
        ));

    screenshots.merge(storybook)
}
