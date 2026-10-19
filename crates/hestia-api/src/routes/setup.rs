//! The Settings page: everything that used to need the command line.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use hestia_freebox::{FreeboxApi, Token, load_token, save_token};
use qrcode::QrCode;
use qrcode::render::svg;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json_, json};
use tokio::time::Instant;
use tracing::info;

use super::CurrentUser;
use crate::auth::{TokenScope, hash_password, valid_name};
use crate::error::ApiError;
use crate::{AppState, Setup};

const MIN_PASSWORD_CHARS: usize = 10;
const AUTHORIZATION_WAIT: Duration = Duration::from_secs(130);
const AUTHORIZATION_POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum FreeboxFlow {
    #[default]
    Idle,
    Pending,
    Granted,
    Failed {
        reason: String,
    },
}

pub(crate) struct SetupState {
    pub config: Setup,
    freebox_flow: Mutex<FreeboxFlow>,
    /// Newer version published on GitHub, from the last check.
    available_update: Mutex<Option<String>>,
}

impl SetupState {
    pub(crate) fn new(config: Setup) -> Self {
        Self {
            config,
            freebox_flow: Mutex::new(FreeboxFlow::Idle),
            available_update: Mutex::new(None),
        }
    }

    pub(crate) fn available_update(&self) -> Option<String> {
        self.available_update
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set_available_update(&self, version: Option<String>) {
        *self
            .available_update
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = version;
    }

    fn flow(&self) -> FreeboxFlow {
        self.freebox_flow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_flow(&self, flow: FreeboxFlow) {
        *self
            .freebox_flow
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = flow;
    }
}

// ---- Apple Home -------------------------------------------------------------------------

pub(crate) async fn homekit(State(state): State<AppState>, _user: CurrentUser) -> Json<Json_> {
    let Some(homekit) = &state.setup.config.homekit else {
        return Json(json!({ "enabled": false }));
    };
    let status = homekit.status();
    Json(json!({
        "enabled": true,
        "name": homekit.name(),
        "paired": status.paired,
        "controllers": status.controllers,
        "accessories": status.accessories,
        // Only useful (and only shown) while nobody is paired.
        "setup_code": (!status.paired).then_some(status.setup_code),
    }))
}

/// The pairing QR code; the iPhone camera recognises it directly.
pub(crate) async fn homekit_qr(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Response, ApiError> {
    let homekit = state
        .setup
        .config
        .homekit
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("pont Apple Home désactivé".to_owned()))?;
    let status = homekit.status();
    if status.paired {
        return Err(ApiError::NotFound("pont déjà appairé".to_owned()));
    }
    let code = QrCode::new(status.setup_uri.as_bytes()).map_err(|_| ApiError::Internal)?;
    let image = code
        .render::<svg::Color<'_>>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        image,
    )
        .into_response())
}

// ---- Freebox ----------------------------------------------------------------------------

pub(crate) async fn freebox(State(state): State<AppState>, _user: CurrentUser) -> Json<Json_> {
    let Some(config) = &state.setup.config.freebox else {
        return Json(json!({ "configured": false }));
    };
    let flow = state.setup.flow();
    let token = load_token(&state.setup.config.data_dir).ok().flatten();
    match FreeboxApi::discover(&config.host, config.port).await {
        Ok((_, version)) => Json(json!({
            "configured": true,
            "reachable": true,
            "model": version.box_model_name,
            "api_version": version.api_version,
            "authorized": token.is_some_and(|token| token.box_uid == version.uid),
            "flow": flow,
        })),
        Err(error) => Json(json!({
            "configured": true,
            "reachable": false,
            "error": format!("{error:#}"),
            "authorized": token.is_some(),
            "flow": flow,
        })),
    }
}

/// Starts the authorisation; the household confirms it on the box within two minutes.
pub(crate) async fn freebox_authorize(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<(StatusCode, Json<Json_>), ApiError> {
    let config = state.setup.config.freebox.clone().ok_or_else(|| {
        ApiError::NotFound("aucun pilote Freebox dans la configuration".to_owned())
    })?;
    if matches!(state.setup.flow(), FreeboxFlow::Pending) {
        return Err(ApiError::Conflict(
            "une demande est déjà en cours".to_owned(),
        ));
    }
    state.setup.set_flow(FreeboxFlow::Pending);
    let started = async {
        let (api, version) = FreeboxApi::discover(&config.host, config.port)
            .await
            .map_err(|error| format!("Freebox injoignable : {error:#}"))?;
        let (app_token, track) = api
            .request_authorization(&device_name())
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((api, version, app_token, track))
    }
    .await;
    let (api, version, app_token, track) = match started {
        Ok(started) => started,
        Err(reason) => {
            state.setup.set_flow(FreeboxFlow::Failed {
                reason: reason.clone(),
            });
            return Err(ApiError::BadGateway(reason));
        }
    };
    info!(user = user.name, "Freebox authorisation requested");
    let setup = Arc::clone(&state.setup);
    tokio::spawn(async move {
        let outcome = wait_for_grant(&api, track).await.and_then(|()| {
            let token = Token {
                app_token,
                box_uid: version.uid,
            };
            save_token(&setup.config.data_dir, &token).map_err(|error| format!("{error:#}"))
        });
        setup.set_flow(match outcome {
            Ok(()) => FreeboxFlow::Granted,
            Err(reason) => FreeboxFlow::Failed { reason },
        });
    });
    Ok((StatusCode::ACCEPTED, Json(json!({ "state": "pending" }))))
}

async fn wait_for_grant(api: &FreeboxApi, track: u64) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let status = api
            .authorization_status(track)
            .await
            .map_err(|error| error.to_string())?;
        match status.as_str() {
            "granted" => return Ok(()),
            "pending" if started.elapsed() < AUTHORIZATION_WAIT => {
                tokio::time::sleep(AUTHORIZATION_POLL).await;
            }
            "pending" | "timeout" => {
                return Err("demande non validée à temps sur la Freebox".to_owned());
            }
            "denied" => return Err("demande refusée sur la Freebox".to_owned()),
            other => return Err(format!("état inattendu de la Freebox : {other}")),
        }
    }
}

fn device_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Hestia".to_owned())
}

// ---- AI access tokens -------------------------------------------------------------------

pub(crate) async fn tokens(State(state): State<AppState>, _user: CurrentUser) -> Json<Json_> {
    let tokens: Vec<Json_> = state
        .auth
        .tokens()
        .list()
        .into_iter()
        .map(|token| json!({ "name": token.name, "control": token.scope == TokenScope::Control }))
        .collect();
    Json(json!(tokens))
}

#[derive(Deserialize)]
pub(crate) struct NewToken {
    name: String,
    #[serde(default)]
    control: bool,
}

pub(crate) async fn create_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewToken>,
) -> Result<(StatusCode, Json<Json_>), ApiError> {
    let name = body.name.trim().to_lowercase();
    if !valid_name(&name) {
        return Err(ApiError::Invalid(
            "nom de 1 à 32 caractères : lettres minuscules, chiffres, « . », « - » ou « _ »"
                .to_owned(),
        ));
    }
    if state
        .auth
        .tokens()
        .list()
        .iter()
        .any(|token| token.name == name)
    {
        return Err(ApiError::Invalid(format!(
            "un jeton « {name} » existe déjà"
        )));
    }
    let scope = if body.control {
        TokenScope::Control
    } else {
        TokenScope::Read
    };
    let token = state
        .auth
        .tokens()
        .create(&name, scope)
        .map_err(|error| ApiError::Invalid(error.to_string()))?;
    info!(user = user.name, token = name, ?scope, "MCP token created");
    Ok((
        StatusCode::CREATED,
        Json(json!({ "name": name, "token": token })),
    ))
}

pub(crate) async fn delete_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .auth
        .tokens()
        .remove(&name)
        .map_err(|_| ApiError::NotFound(format!("jeton « {name} » inconnu")))?;
    info!(user = user.name, token = name, "MCP token revoked");
    Ok(StatusCode::NO_CONTENT)
}

// ---- Accounts ---------------------------------------------------------------------------

pub(crate) async fn users(State(state): State<AppState>, user: CurrentUser) -> Json<Json_> {
    Json(json!({ "current": user.name, "users": state.auth.users().names() }))
}

#[derive(Deserialize)]
pub(crate) struct NewUser {
    name: String,
    password: String,
}

pub(crate) async fn create_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewUser>,
) -> Result<StatusCode, ApiError> {
    let name = body.name.trim().to_lowercase();
    if !valid_name(&name) {
        return Err(ApiError::Invalid(
            "identifiant de 1 à 32 caractères : lettres minuscules, chiffres, « . », « - » ou « _ »".to_owned(),
        ));
    }
    if state.auth.users().contains(&name) {
        return Err(ApiError::Invalid(format!(
            "le compte « {name} » existe déjà"
        )));
    }
    let hash = new_password_hash(body.password).await?;
    state
        .auth
        .users()
        .add(&name, hash)
        .map_err(|error| ApiError::Invalid(error.to_string()))?;
    info!(user = user.name, created = name, "account created");
    Ok(StatusCode::CREATED)
}

pub(crate) async fn delete_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    if name == user.name {
        return Err(ApiError::Invalid(
            "tu ne peux pas supprimer ton propre compte".to_owned(),
        ));
    }
    state
        .auth
        .users()
        .remove(&name)
        .map_err(|_| ApiError::NotFound(format!("compte « {name} » inconnu")))?;
    info!(user = user.name, removed = name, "account removed");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct PasswordChange {
    current: String,
    new: String,
}

pub(crate) async fn change_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<PasswordChange>,
) -> Result<StatusCode, ApiError> {
    if !state.auth.check_password(&user.name, body.current).await {
        return Err(ApiError::Invalid(
            "mot de passe actuel incorrect".to_owned(),
        ));
    }
    let hash = new_password_hash(body.new).await?;
    state
        .auth
        .users()
        .set_password(&user.name, hash)
        .map_err(|error| ApiError::Invalid(error.to_string()))?;
    info!(user = user.name, "password changed");
    Ok(StatusCode::NO_CONTENT)
}

async fn new_password_hash(password: String) -> Result<String, ApiError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ApiError::Invalid(format!(
            "le mot de passe doit faire au moins {MIN_PASSWORD_CHARS} caractères"
        )));
    }
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|_| ApiError::Internal)?
        .map_err(|_| ApiError::Internal)
}
