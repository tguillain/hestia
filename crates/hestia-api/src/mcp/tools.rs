use std::fmt::Write;

use hestia_core::{Command, DeviceId, Origin, Value};
use serde_json::{Value as Json, json};
use tracing::info;

use super::RpcError;
use crate::AppState;
use crate::auth::{TokenInfo, TokenScope};

const MAX_HISTORY_HOURS: u32 = 240;

pub(crate) fn definitions(scope: TokenScope) -> Vec<Json> {
    let device = json!({"type": "string", "description": "Device id from list_devices, e.g. govee:a1b2c3d4e5f60718"});
    let point = json!({"type": "string", "description": "Point key from the device, e.g. on, brightness, players"});
    let mut tools = vec![
        json!({
            "name": "list_devices",
            "title": "Lister les appareils",
            "description": "All devices with their room, availability and current values.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true, "openWorldHint": false},
        }),
        json!({
            "name": "get_device",
            "title": "Détail d'un appareil",
            "description": "One device with every point: kind, range, unit, writability and current value.",
            "inputSchema": {"type": "object", "properties": {"device": device}, "required": ["device"]},
            "annotations": {"readOnlyHint": true, "openWorldHint": false},
        }),
        json!({
            "name": "get_history",
            "title": "Historique d'un point",
            "description": "Averages of a numeric or on/off point over the last hours, in equal time slices \
                            (on/off points are 0/1: averages are the share of time on).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": device,
                    "point": point,
                    "hours": {"type": "integer", "minimum": 1, "maximum": MAX_HISTORY_HOURS, "default": 24},
                    "slices": {"type": "integer", "minimum": 10, "maximum": 200, "default": 48}
                },
                "required": ["device", "point"]
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false},
        }),
        json!({
            "name": "check_update",
            "title": "Chercher une mise à jour",
            "description": "Compares the running Hestia version with the latest signed release published on GitHub.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true, "openWorldHint": true},
        }),
    ];
    if scope == TokenScope::Control {
        tools.push(json!({
            "name": "install_update",
            "title": "Mettre Hestia à jour",
            "description": "Downloads the latest release published on GitHub, checks its signature, installs it and \
                            restarts Hestia (about 10 seconds without dashboard or Apple Home). Only when the user \
                            asked for an update.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true},
        }));
        tools.push(json!({
            "name": "set_point",
            "title": "Commander un appareil",
            "description": "Changes a writable point, e.g. turn a light on ({\"device\":…,\"point\":\"on\",\"value\":true}) \
                            or set brightness to 40 ({\"point\":\"brightness\",\"value\":40}). \
                            Colours are {\"r\":255,\"g\":120,\"b\":0}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": device,
                    "point": point,
                    "value": {"description": "Boolean, number, {r,g,b} colour or enumeration string"}
                },
                "required": ["device", "point", "value"]
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        }));
    }
    tools
}

pub(crate) async fn call(
    state: &AppState,
    token: &TokenInfo,
    params: &Json,
) -> Result<Json, RpcError> {
    let name = params["name"].as_str().unwrap_or_default();
    let arguments = &params["arguments"];
    let outcome = match name {
        "list_devices" => Ok(list_devices(state)),
        "get_device" => get_device(state, arguments),
        "get_history" => get_history(state, arguments).await,
        "set_point" if token.scope == TokenScope::Control => {
            set_point(state, token, arguments).await
        }
        "set_point" | "install_update" if token.scope != TokenScope::Control => {
            Err("this token is read-only".to_owned())
        }
        "check_update" => crate::routes::latest_report(state)
            .await
            .map_err(|error| error.message()),
        "install_update" => {
            info!(token = token.name, "MCP update requested");
            crate::routes::install_latest(state, &format!("mcp:{}", token.name))
                .await
                .map(|version| {
                    json!({"installed": version, "note": "Hestia redémarre, il répond de nouveau d'ici une dizaine de secondes."})
                })
                .map_err(|error| error.message())
        }
        _ => return Err(RpcError::invalid_params(format!("unknown tool {name:?}"))),
    };
    Ok(match outcome {
        Ok(result) => {
            json!({"content": [{"type": "text", "text": result.to_string()}], "isError": false})
        }
        Err(message) => json!({"content": [{"type": "text", "text": message}], "isError": true}),
    })
}

fn list_devices(state: &AppState) -> Json {
    let devices: Vec<Json> = state
        .hub
        .devices()
        .into_iter()
        .map(|device| {
            let points: Vec<Json> = device
                .points
                .iter()
                .map(|point| {
                    json!({
                        "key": point.spec.key,
                        "label": point.spec.label,
                        "value": point.value,
                        "unit": point.spec.unit.map(hestia_core::Unit::symbol),
                        "writable": point.spec.is_writable(),
                    })
                })
                .collect();
            json!({
                "id": device.id,
                "name": device.name,
                "room": device.room,
                "model": device.model,
                "available": device.available,
                "points": points,
            })
        })
        .collect();
    json!(devices)
}

fn device_id(arguments: &Json) -> Result<DeviceId, String> {
    arguments["device"]
        .as_str()
        .ok_or_else(|| "missing device".to_owned())?
        .parse()
        .map_err(|_| "invalid device id".to_owned())
}

fn get_device(state: &AppState, arguments: &Json) -> Result<Json, String> {
    let id = device_id(arguments)?;
    state
        .hub
        .device(&id)
        .map(|device| json!(device))
        .ok_or_else(|| format!("unknown device {id}"))
}

async fn get_history(state: &AppState, arguments: &Json) -> Result<Json, String> {
    let history = state
        .history
        .clone()
        .ok_or_else(|| "history is disabled".to_owned())?;
    let id = device_id(arguments)?;
    let point = arguments["point"]
        .as_str()
        .ok_or_else(|| "missing point".to_owned())?
        .to_owned();
    let hours = arguments["hours"]
        .as_u64()
        .and_then(|hours| u32::try_from(hours).ok())
        .unwrap_or(24)
        .clamp(1, MAX_HISTORY_HOURS);
    let slices = arguments["slices"]
        .as_u64()
        .and_then(|slices| usize::try_from(slices).ok())
        .unwrap_or(48)
        .clamp(10, 200);
    let (query_id, query_point) = (id.clone(), point.clone());
    let buckets =
        tokio::task::spawn_blocking(move || history.query(&query_id, &query_point, hours, slices))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("{error:#}"))?;
    let values: Vec<Json> = buckets
        .values
        .iter()
        .map(|value| value.map_or(Json::Null, |value| json!((value * 100.0).round() / 100.0)))
        .collect();
    Ok(json!({
        "device": id,
        "point": point,
        "from_unix_ms": buckets.from,
        "slice_minutes": buckets.step / 60_000,
        "values": values,
    }))
}

async fn set_point(state: &AppState, token: &TokenInfo, arguments: &Json) -> Result<Json, String> {
    let device = device_id(arguments)?;
    let point = arguments["point"]
        .as_str()
        .ok_or_else(|| "missing point".to_owned())?
        .to_owned();
    let value: Value = serde_json::from_value(arguments["value"].clone()).map_err(|_| {
        "value must be a boolean, a number, a {r,g,b} colour or a string".to_owned()
    })?;
    let mut summary = String::new();
    let _ = write!(summary, "{device} {point} = {}", arguments["value"]);
    info!(token = token.name, command = summary, "MCP command");
    state
        .hub
        .command(
            Command {
                device,
                point,
                value,
            },
            Origin::Mcp,
        )
        .await
        .map(|()| json!({"done": true}))
        .map_err(|error| error.to_string())
}
