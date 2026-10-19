use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request};
use axum::http::header::{HeaderName, ORIGIN};
use axum::http::{HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::client::Client;
use crate::error::ApiError;

const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; \
     script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub(crate) async fn headers(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let mut set = |name: &'static str, value: &'static str| {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    };
    set("content-security-policy", CONTENT_SECURITY_POLICY);
    set("x-content-type-options", "nosniff");
    set("x-frame-options", "DENY");
    set("referrer-policy", "no-referrer");
    if is_api {
        set("cache-control", "no-store");
    }
    response
}

/// Refuses writes whose `Origin` is another site. With `SameSite=Strict` cookies and JSON-only
/// bodies this closes cross-site request forgery.
pub(crate) async fn same_origin_writes(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if !safe && let Some(origin) = request.headers().get(ORIGIN) {
        let host = Client::of(peer, request.headers()).host;
        let origin_host = origin
            .to_str()
            .ok()
            .and_then(|origin| origin.split_once("://"))
            .map(|(_, authority)| authority);
        if origin_host.is_none() || origin_host != host.as_deref() {
            return ApiError::Forbidden.into_response();
        }
    }
    next.run(request).await
}
