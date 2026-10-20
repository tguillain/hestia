//! Scenes and rules: edited in the Settings page, scenes started from the dashboard.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hestia_automation::{Automations, Document, Scene};
use hestia_core::{DeviceId, Origin};
use serde::Deserialize;
use serde_json::{Value as Json_, json};
use tracing::info;

use super::CurrentUser;
use crate::AppState;
use crate::error::ApiError;

pub(crate) async fn get(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<Json_>, ApiError> {
    let automations = enabled(&state)?;
    let sun = automations
        .sun_today()
        .map(|(sunrise, sunset)| json!({ "sunrise": sunrise, "sunset": sunset }));
    Ok(Json(
        json!({ "document": automations.document(), "sun": sun }),
    ))
}

pub(crate) async fn replace(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(document): Json<Document>,
) -> Result<Json<Document>, ApiError> {
    let saved = enabled(&state)?
        .replace(document)
        .map_err(ApiError::Invalid)?;
    info!(
        user = user.name,
        scenes = saved.scenes.len(),
        rules = saved.rules.len(),
        "automations saved"
    );
    Ok(Json(saved))
}

#[derive(Deserialize)]
pub(crate) struct NewScene {
    name: String,
    devices: Vec<DeviceId>,
}

/// Records a scene from what the chosen lamps show now.
pub(crate) async fn capture(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewScene>,
) -> Result<(StatusCode, Json<Scene>), ApiError> {
    let scene = enabled(&state)?
        .capture_scene(&body.name, &body.devices)
        .map_err(ApiError::Invalid)?;
    info!(user = user.name, scene = scene.name, "scene recorded");
    Ok((StatusCode::CREATED, Json(scene)))
}

pub(crate) async fn run_scene(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let automations = enabled(&state)?;
    if automations.document().scene(&id).is_none() {
        return Err(ApiError::NotFound(format!("scène « {id} » inconnue")));
    }
    automations
        .run_scene(&id, Origin::Ui)
        .await
        .map_err(ApiError::BadGateway)?;
    info!(user = user.name, scene = id, "scene run");
    Ok(StatusCode::NO_CONTENT)
}

fn enabled(state: &AppState) -> Result<&Arc<Automations>, ApiError> {
    state
        .setup
        .config
        .automations
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("automatisations indisponibles".to_owned()))
}
