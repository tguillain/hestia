//! Looking for new devices, and lamps added by their address.

use std::net::Ipv4Addr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hestia_govee::{add_manual_address, load_known, remove_manual_address};
use serde::Deserialize;
use serde_json::{Value as Json_, json};
use tracing::info;

use super::CurrentUser;
use crate::AppState;
use crate::error::ApiError;

/// Asks every driver able to discover to look for devices now.
pub(crate) async fn discover(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<StatusCode, ApiError> {
    let sender = state
        .setup
        .config
        .discovery
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("découverte indisponible".to_owned()))?;
    sender.send_modify(|requests| *requests += 1);
    info!(user = user.name, "device discovery requested");
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn govee(State(state): State<AppState>, _user: CurrentUser) -> Json<Json_> {
    let known = load_known(&state.setup.config.data_dir);
    Json(json!({ "manual": known.manual, "seen": known.seen }))
}

#[derive(Deserialize)]
pub(crate) struct NewAddress {
    address: String,
}

pub(crate) async fn add_address(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewAddress>,
) -> Result<StatusCode, ApiError> {
    let address = parse_private(&body.address)?;
    add_manual_address(&state.setup.config.data_dir, address).map_err(|_| ApiError::Internal)?;
    if let Some(sender) = &state.setup.config.discovery {
        sender.send_modify(|requests| *requests += 1);
    }
    info!(user = user.name, %address, "Govee address added");
    Ok(StatusCode::CREATED)
}

pub(crate) async fn remove_address(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(address): Path<String>,
) -> Result<StatusCode, ApiError> {
    let address = parse_private(&address)?;
    let removed = remove_manual_address(&state.setup.config.data_dir, address)
        .map_err(|_| ApiError::Internal)?;
    if !removed {
        return Err(ApiError::NotFound(format!("adresse {address} inconnue")));
    }
    info!(user = user.name, %address, "Govee address removed");
    Ok(StatusCode::NO_CONTENT)
}

/// Only addresses of the home network make sense for a lamp.
fn parse_private(raw: &str) -> Result<Ipv4Addr, ApiError> {
    let address: Ipv4Addr = raw
        .trim()
        .parse()
        .map_err(|_| ApiError::Invalid(format!("« {raw} » n'est pas une adresse IPv4")))?;
    if address.is_private() {
        Ok(address)
    } else {
        Err(ApiError::Invalid(format!(
            "{address} n'est pas une adresse du réseau local (192.168.x.x, 10.x.x.x…)"
        )))
    }
}
