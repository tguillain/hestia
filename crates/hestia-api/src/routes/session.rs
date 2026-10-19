use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::auth::{LoginError, SESSION_COOKIE};
use crate::client::Client;
use crate::error::ApiError;

const COOKIE_MAX_AGE_S: u32 = 30 * 24 * 3600;

/// Extractor for routes that require a logged-in household member.
pub(crate) struct CurrentUser {
    pub name: String,
    token: String,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(current_user(parts, state))
    }
}

fn current_user(parts: &Parts, state: &AppState) -> Result<CurrentUser, ApiError> {
    let token = session_token(&parts.headers).ok_or(ApiError::Unauthorized)?;
    let name = state
        .auth
        .authenticate(&token)
        .ok_or(ApiError::Unauthorized)?;
    Ok(CurrentUser { name, token })
}

#[derive(Deserialize)]
pub(crate) struct Credentials {
    username: String,
    password: String,
}

pub(crate) async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> Result<Response, ApiError> {
    let client = Client::of(peer, &headers);
    let username = credentials.username.trim().to_lowercase();
    let token = state
        .auth
        .login(client.ip, username.clone(), credentials.password)
        .await
        .map_err(|error| match error {
            LoginError::Locked(wait) => ApiError::TooMany(Some(wait)),
            LoginError::Invalid => ApiError::Unauthorized,
            LoginError::Internal => ApiError::Internal,
        })?;
    let secure = if client.https { "; Secure" } else { "" };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE_S}{secure}"
    );
    let cookie = HeaderValue::from_str(&cookie).map_err(|_| ApiError::Internal)?;
    Ok(([(SET_COOKIE, cookie)], Json(json!({ "user": username }))).into_response())
}

pub(crate) async fn logout(State(state): State<AppState>, user: CurrentUser) -> Response {
    state.auth.logout(&user.token);
    let expired =
        HeaderValue::from_static("hestia_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    (StatusCode::NO_CONTENT, [(SET_COOKIE, expired)]).into_response()
}

pub(crate) async fn current(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Json<serde_json::Value> {
    Json(json!({ "user": user.name, "update": state.setup.available_update() }))
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty())
}
