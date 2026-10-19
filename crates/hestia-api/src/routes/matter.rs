//! Pairing Matter devices from the Settings page.

use std::net::{IpAddr, SocketAddr};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hestia_core::DeviceId;
use hestia_matter::{MATTER_PORT, PairedSummary, paired_devices};
use serde::Deserialize;
use serde_json::{Value as Json_, json};
use tracing::info;

use super::CurrentUser;
use crate::error::ApiError;
use crate::{AppState, MatterSetup};

pub(crate) async fn list(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<Json_>, ApiError> {
    let Some(matter) = &state.setup.config.matter else {
        return Ok(Json(json!({ "enabled": false })));
    };
    let devices = paired_devices(&state.setup.config.data_dir, &matter.instance)
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(json!({ "enabled": true, "devices": devices })))
}

#[derive(Deserialize)]
pub(crate) struct NewDevice {
    code: String,
    /// For networks where mDNS does not get through.
    #[serde(default)]
    address: Option<String>,
}

/// Answers once the device is paired, which can take up to two minutes.
pub(crate) async fn pair(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewDevice>,
) -> Result<(StatusCode, Json<PairedSummary>), ApiError> {
    let matter = enabled(&state)?;
    let address = match body.address.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(SocketAddr::new(parse_private(raw)?, MATTER_PORT)),
    };
    info!(user = user.name, "Matter pairing requested");
    let paired = matter
        .pairing
        .pair(body.code, address)
        .await
        .map_err(ApiError::Invalid)?;
    Ok((StatusCode::CREATED, Json(paired)))
}

pub(crate) async fn remove(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let matter = enabled(&state)?;
    let id: DeviceId = id
        .parse()
        .map_err(|_| ApiError::NotFound(format!("appareil {id} inconnu")))?;
    if id.instance() != matter.instance {
        return Err(ApiError::NotFound(format!("appareil {id} inconnu")));
    }
    matter
        .pairing
        .remove(id.native().to_owned())
        .await
        .map_err(ApiError::NotFound)?;
    info!(user = user.name, device = %id, "Matter device removed");
    Ok(StatusCode::NO_CONTENT)
}

fn enabled(state: &AppState) -> Result<&MatterSetup, ApiError> {
    state
        .setup
        .config
        .matter
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("pilote Matter non configuré".to_owned()))
}

/// Only addresses of the home network make sense for a device.
fn parse_private(raw: &str) -> Result<IpAddr, ApiError> {
    let address: IpAddr = raw
        .parse()
        .map_err(|_| ApiError::Invalid(format!("« {raw} » n'est pas une adresse IP")))?;
    let local = match address {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
    };
    if local {
        Ok(address)
    } else {
        Err(ApiError::Invalid(format!(
            "{address} n'est pas une adresse du réseau local"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_private;

    #[test]
    fn only_local_addresses() {
        assert!(parse_private("192.168.1.40").is_ok());
        assert!(parse_private("fd00::12").is_ok());
        assert!(parse_private("8.8.8.8").is_err());
        assert!(parse_private("lampe").is_err());
    }
}
