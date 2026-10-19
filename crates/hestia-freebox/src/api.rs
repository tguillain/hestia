//! Freebox OS HTTP API on the LAN: discovery, app authorisation, session login and calls.

use std::fmt::Write;
use std::time::Duration;

use anyhow::{Context, anyhow};
use hmac::{Hmac, Mac};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};
use sha1::Sha1;
use tokio::time::timeout;

pub const APP_ID: &str = "fr.hestia";
const APP_NAME: &str = "Hestia";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("session expired")]
    Session,
    #[error("missing permission: {0}")]
    Permission(String),
    #[error("Freebox refused: {0}")]
    Refused(String),
    #[error(transparent)]
    Transport(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiVersion {
    pub uid: String,
    pub api_version: String,
    pub api_base_url: String,
    #[serde(default)]
    pub box_model_name: String,
}

#[derive(Deserialize)]
struct Envelope {
    success: bool,
    #[serde(default)]
    result: Json,
    #[serde(default)]
    error_code: String,
    #[serde(default)]
    msg: String,
}

pub struct FreeboxApi {
    http: Client<HttpConnector, Full<Bytes>>,
    base: String,
    session: Option<String>,
}

impl FreeboxApi {
    /// Reads `/api_version` (no authentication) and prepares the versioned base URL.
    ///
    /// # Errors
    /// Fails when the Freebox does not answer.
    pub async fn discover(host: &str, port: u16) -> anyhow::Result<(Self, ApiVersion)> {
        let mut connector = HttpConnector::new();
        connector.set_connect_timeout(Some(Duration::from_secs(5)));
        let http = Client::builder(TokioExecutor::new()).build(connector);
        let mut api = Self {
            http,
            base: String::new(),
            session: None,
        };
        let raw = api
            .send(
                Method::GET,
                &format!("http://{host}:{port}/api_version"),
                None,
            )
            .await?;
        let version: ApiVersion = serde_json::from_slice(&raw).context("invalid /api_version")?;
        let major = version.api_version.split('.').next().unwrap_or("8");
        api.base = format!("http://{host}:{port}{}v{major}/", version.api_base_url);
        Ok((api, version))
    }

    /// # Errors
    /// [`ApiError::Session`] when the session expired, [`ApiError::Permission`] when the
    /// application lacks a right, otherwise why the call failed.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.call(Method::GET, path, None).await
    }

    /// # Errors
    /// [`ApiError::Session`] when the session expired, [`ApiError::Permission`] when the
    /// application lacks a right, otherwise why the call failed.
    pub async fn put<T: DeserializeOwned>(&self, path: &str, body: &Json) -> Result<T, ApiError> {
        self.call(Method::PUT, path, Some(body)).await
    }

    /// # Errors
    /// [`ApiError::Session`] when the session expired, [`ApiError::Permission`] when the
    /// application lacks a right, otherwise why the call failed.
    pub async fn post<T: DeserializeOwned>(&self, path: &str, body: &Json) -> Result<T, ApiError> {
        self.call(Method::POST, path, Some(body)).await
    }

    /// Opens a session from the app token granted at registration.
    ///
    /// # Errors
    /// Fails when the token was revoked or the box is unreachable.
    pub async fn login(&mut self, app_token: &str) -> Result<(), ApiError> {
        #[derive(Deserialize)]
        struct Challenge {
            challenge: String,
        }
        #[derive(Deserialize)]
        struct Session {
            session_token: String,
        }
        self.session = None;
        let Challenge { challenge } = self.get("login/").await?;
        let session: Session = self
            .post(
                "login/session/",
                &json!({ "app_id": APP_ID, "password": password(app_token, &challenge) }),
            )
            .await?;
        self.session = Some(session.session_token);
        Ok(())
    }

    /// Asks the box to authorise this application; the user confirms on the Freebox.
    ///
    /// # Errors
    /// Fails when the request is refused.
    pub async fn request_authorization(
        &self,
        device_name: &str,
    ) -> Result<(String, u64), ApiError> {
        #[derive(Deserialize)]
        struct Authorization {
            app_token: String,
            track_id: u64,
        }
        let granted: Authorization = self
            .post(
                "login/authorize/",
                &json!({
                    "app_id": APP_ID,
                    "app_name": APP_NAME,
                    "app_version": env!("CARGO_PKG_VERSION"),
                    "device_name": device_name,
                }),
            )
            .await?;
        Ok((granted.app_token, granted.track_id))
    }

    /// `pending`, `granted`, `denied`, `timeout` or `unknown`.
    ///
    /// # Errors
    /// Fails when the box is unreachable.
    pub async fn authorization_status(&self, track_id: u64) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Track {
            status: String,
        }
        let track: Track = self.get(&format!("login/authorize/{track_id}")).await?;
        Ok(track.status)
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
    ) -> Result<T, ApiError> {
        let raw = self
            .send(method, &format!("{}{path}", self.base), body)
            .await?;
        let envelope: Envelope = serde_json::from_slice(&raw)
            .map_err(|error| ApiError::Transport(anyhow!("invalid answer for {path}: {error}")))?;
        if !envelope.success {
            return Err(match envelope.error_code.as_str() {
                "auth_required" | "invalid_session" => ApiError::Session,
                "insufficient_rights" => ApiError::Permission(envelope.msg),
                _ => ApiError::Refused(format!("{} ({})", envelope.msg, envelope.error_code)),
            });
        }
        serde_json::from_value(envelope.result)
            .map_err(|error| ApiError::Transport(anyhow!("unexpected result for {path}: {error}")))
    }

    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&Json>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut request = Request::builder()
            .method(method)
            .uri(url)
            .header("user-agent", APP_NAME);
        if let Some(session) = &self.session {
            request = request.header("X-Fbx-App-Auth", session);
        }
        let payload = match body {
            Some(json) => {
                request = request.header("content-type", "application/json");
                Full::new(Bytes::from(json.to_string()))
            }
            None => Full::new(Bytes::new()),
        };
        let response = timeout(REQUEST_TIMEOUT, self.http.request(request.body(payload)?))
            .await
            .context("Freebox did not answer in time")??;
        let collected = timeout(
            REQUEST_TIMEOUT,
            Limited::new(response.into_body(), MAX_RESPONSE).collect(),
        )
        .await
        .context("Freebox answer too slow")?
        .map_err(|error| anyhow!("cannot read Freebox answer: {error}"))?;
        Ok(collected.to_bytes().to_vec())
    }
}

/// Session password: HMAC-SHA1 of the login challenge, keyed by the app token, in hex.
///
/// # Panics
/// Never: HMAC accepts keys of any length.
#[must_use]
pub fn password(app_token: &str, challenge: &str) -> String {
    let mut mac =
        Hmac::<Sha1>::new_from_slice(app_token.as_bytes()).expect("HMAC accepts any key length");
    mac.update(challenge.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::password;

    #[test]
    fn password_is_rfc_hmac_sha1() {
        assert_eq!(
            password("key", "The quick brown fox jumps over the lazy dog"),
            "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9"
        );
    }
}
