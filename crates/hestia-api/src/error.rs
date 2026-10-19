use std::time::Duration;

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hestia_hub::CommandError;
use serde_json::json;

#[derive(Debug)]
pub(crate) enum ApiError {
    Unauthorized,
    Forbidden,
    NotFound(String),
    Invalid(String),
    Conflict(String),
    TooMany(Option<Duration>),
    Timeout,
    BadGateway(String),
    Internal,
}

impl ApiError {
    /// The human-readable explanation, for clients that are not HTTP (MCP tools).
    pub(crate) fn message(&self) -> String {
        self.parts().2
    }

    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Connexion requise".to_owned(),
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "Requête refusée".to_owned(),
            ),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, "not_found", message.clone()),
            Self::Invalid(message) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid", message.clone())
            }
            Self::Conflict(message) => (StatusCode::CONFLICT, "unavailable", message.clone()),
            Self::TooMany(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                "too_many",
                "Trop de tentatives".to_owned(),
            ),
            Self::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "L'appareil n'a pas répondu à temps".to_owned(),
            ),
            Self::BadGateway(message) => (StatusCode::BAD_GATEWAY, "device_error", message.clone()),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Erreur interne".to_owned(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        let mut response =
            (status, Json(json!({ "error": code, "message": message }))).into_response();
        if let Self::TooMany(Some(wait)) = self
            && let Ok(value) = HeaderValue::from_str(&wait.as_secs().max(1).to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

impl From<CommandError> for ApiError {
    fn from(error: CommandError) -> Self {
        let message = error.to_string();
        match error {
            CommandError::UnknownDevice(_) | CommandError::UnknownPoint { .. } => {
                Self::NotFound(message)
            }
            CommandError::ReadOnly(_) | CommandError::Invalid(_) => Self::Invalid(message),
            CommandError::Unavailable(_) => Self::Conflict(message),
            CommandError::Busy => Self::TooMany(None),
            CommandError::Timeout => Self::Timeout,
            CommandError::DriverStopped | CommandError::Failed(_) => Self::BadGateway(message),
        }
    }
}
