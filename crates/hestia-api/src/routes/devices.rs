use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hestia_core::{Command, DeviceId, Origin, Value};
use hestia_hub::DeviceSnapshot;
use serde::Deserialize;

use super::CurrentUser;
use crate::AppState;
use crate::error::ApiError;

pub(crate) async fn list(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Json<Vec<DeviceSnapshot>> {
    Json(state.hub.devices())
}

pub(crate) async fn one(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<DeviceSnapshot>, ApiError> {
    let id = parse_id(&id)?;
    state
        .hub
        .device(&id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("unknown device {id}")))
}

#[derive(Deserialize)]
pub(crate) struct SetPoint {
    value: Value,
}

pub(crate) async fn set_point(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, point)): Path<(String, String)>,
    Json(body): Json<SetPoint>,
) -> Result<StatusCode, ApiError> {
    let command = Command {
        device: parse_id(&id)?,
        point,
        value: body.value,
    };
    tracing::debug!(user = user.name, device = %command.device, point = command.point, "command from dashboard");
    state.hub.command(command, Origin::Ui).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn parse_id(raw: &str) -> Result<DeviceId, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::NotFound(format!("unknown device {raw}")))
}
