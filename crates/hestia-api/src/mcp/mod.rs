//! Model Context Protocol over "Streamable HTTP": one JSON-RPC message per POST to `/mcp`,
//! answered with plain JSON. Stateless: no session id, no server-initiated stream.

mod tools;

use std::net::SocketAddr;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value as Json_, json};

use crate::AppState;
use crate::auth::{LoginError, TokenInfo};
use crate::client::Client;
use crate::error::ApiError;

/// Newest first; the client's version is echoed back when supported.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

const INSTRUCTIONS: &str = "Home automation hub of a house in France (labels are in French). \
Call list_devices first. Device ids look like `govee:a1b2c3d4e5f60718`; each device has points \
(on, brightness, color, temperature…) with a kind, range and unit. Values are JSON booleans, \
numbers within the point range, colours as {\"r\":0-255,\"g\":0-255,\"b\":0-255}, or strings for \
enumerations. Only change devices when the user asked for it.";

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: Option<Json_>,
    method: String,
    #[serde(default)]
    params: Json_,
}

pub(crate) struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }
}

pub(crate) async fn handle(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = Client::of(peer, &headers);
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let token = match state.auth.mcp_token(client.ip, bearer) {
        Ok(token) => token,
        Err(LoginError::Locked(wait)) => return ApiError::TooMany(Some(wait)).into_response(),
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                [(WWW_AUTHENTICATE, "Bearer")],
                Json(json!({"error": "invalid token"})),
            )
                .into_response();
        }
    };
    let Ok(request) = serde_json::from_slice::<Request>(&body) else {
        let error = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}});
        return (StatusCode::BAD_REQUEST, Json(error)).into_response();
    };
    let Some(id) = request.id.clone() else {
        // Notifications (e.g. notifications/initialized) need no answer.
        return StatusCode::ACCEPTED.into_response();
    };
    let reply = match dispatch(&state, &token, request).await {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": error.code, "message": error.message}})
        }
    };
    Json(reply).into_response()
}

pub(crate) async fn not_allowed() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn dispatch(
    state: &AppState,
    token: &TokenInfo,
    request: Request,
) -> Result<Json_, RpcError> {
    match request.method.as_str() {
        "initialize" => {
            let asked = request.params["protocolVersion"]
                .as_str()
                .unwrap_or_default();
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|version| **version == asked)
                .unwrap_or(&PROTOCOL_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "hestia", "title": "Hestia", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::definitions(token.scope) })),
        "tools/call" => tools::call(state, token, &request.params).await,
        _ => Err(RpcError {
            code: -32601,
            message: format!("Method not found: {}", request.method),
        }),
    }
}
