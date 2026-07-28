//! Origin ヘッダによる CSRF 対策。
//!
//! 本番はセッション Cookie が `SameSite=None` のため、`multipart/form-data` のように
//! preflight なしで送れるリクエストはクロスサイトのフォームから発行できてしまう。
//! CORS はレスポンスの読み取りを防ぐだけで副作用の実行は防がないため、
//! 状態変更メソッドで Origin ヘッダが付いている場合は許可フロントエンド origin か
//! API 自身の origin（Host 一致。Scalar UI 等）以外を 403 で拒否する。
//! Origin なし（CLI・サーバー間・webhook）は素通しする。
//!
//! PAT（`Authorization: Bearer <token>`）はブラウザが自動付与する資格情報ではなく、
//! クロスサイトの攻撃者ページから偽装できない（`Authorization` ヘッダ付きのクロスオリジン
//! リクエストは常に CORS プリフライトの対象になり、許可外オリジンなら実リクエスト自体が
//! ブラウザにブロックされる）ため CSRF の対象外。Bearer ヘッダが付いているリクエストは
//! Origin 検査そのものをスキップする。セッション Cookie 専用の extractor（`CurrentUser` 等）
//! を使うエンドポイントに誤って Bearer ヘッダが付いていても、そちらは Bearer を無視して
//! Cookie セッションを要求するため安全性は変わらない。

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{AppState, error::AppError};

/// `Authorization: Bearer <token>` が付いているか（PAT 経路かどうかの判定）。
fn has_bearer_token(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .is_some_and(|token| !token.is_empty())
}

pub async fn csrf_origin_check(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    if has_bearer_token(req.headers()) {
        return next.run(req).await;
    }
    let Some(origin) = req.headers().get(header::ORIGIN) else {
        return next.run(req).await;
    };

    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().authority().map(|a| a.as_str()));
    let allowed = origin
        .to_str()
        .ok()
        .is_some_and(|origin| origin_allowed(origin, &state.settings.allow_origin, host));

    if allowed {
        next.run(req).await
    } else {
        AppError::Forbidden.into_response()
    }
}

/// Origin が許可フロントエンド origin か、API 自身の origin（authority が Host と一致）か。
fn origin_allowed(origin: &str, allow_origin: &str, host: Option<&str>) -> bool {
    if origin == allow_origin {
        return true;
    }
    match (origin.split_once("://"), host) {
        (Some((_scheme, authority)), Some(host)) => authority == host,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOW: &str = "http://localhost:3000";

    #[test]
    fn allows_configured_frontend_origin() {
        assert!(origin_allowed(
            "http://localhost:3000",
            ALLOW,
            Some("localhost:3400")
        ));
    }

    #[test]
    fn allows_same_origin_as_api_host() {
        assert!(origin_allowed(
            "http://localhost:3400",
            ALLOW,
            Some("localhost:3400")
        ));
        assert!(origin_allowed(
            "https://api.example.com",
            "https://app.example.com",
            Some("api.example.com")
        ));
    }

    #[test]
    fn rejects_cross_site_origin() {
        assert!(!origin_allowed(
            "https://evil.example",
            ALLOW,
            Some("localhost:3400")
        ));
    }

    #[test]
    fn rejects_null_origin() {
        // サンドボックス化された iframe のフォーム送信は Origin: null になる
        assert!(!origin_allowed("null", ALLOW, Some("localhost:3400")));
    }

    #[test]
    fn rejects_when_host_unknown() {
        assert!(!origin_allowed("http://localhost:3400", ALLOW, None));
    }

    #[test]
    fn rejects_port_mismatch() {
        assert!(!origin_allowed(
            "http://localhost:9999",
            ALLOW,
            Some("localhost:3400")
        ));
    }

    fn headers_with_authorization(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, value.parse().unwrap());
        headers
    }

    #[test]
    fn detects_pat_bearer_token() {
        // PAT 経路（Authorization: Bearer）は Origin 検査の対象外にする
        assert!(has_bearer_token(&headers_with_authorization(
            "Bearer pat_abc123"
        )));
    }

    #[test]
    fn ignores_empty_bearer_token() {
        assert!(!has_bearer_token(&headers_with_authorization("Bearer ")));
    }

    #[test]
    fn ignores_non_bearer_authorization() {
        // Basic 認証等、Bearer 以外は PAT 経路とみなさない
        assert!(!has_bearer_token(&headers_with_authorization(
            "Basic dXNlcjpwYXNz"
        )));
    }

    #[test]
    fn ignores_missing_authorization_header() {
        assert!(!has_bearer_token(&HeaderMap::new()));
    }
}
