//! The accessory database: a bridge (aid 1) and one Lightbulb accessory per remembered
//! light, with a fixed instance-id layout. Values come live from the hub.

use std::collections::BTreeMap;

use hestia_core::{Command, DeviceId, Origin, Semantic, Value};
use hestia_hub::{DeviceSnapshot, Hub};
use serde_json::{Value as Json, json};
use tracing::debug;

use crate::color::{hs_to_rgb, kelvin_to_mired, kelvin_to_rgb, mired_to_kelvin, rgb_to_hs};
use crate::state::StoredLight;

pub const BRIDGE_AID: u64 = 1;
const FIRMWARE: &str = env!("CARGO_PKG_VERSION");

pub mod iid {
    pub const IDENTIFY: u64 = 2;
    pub const MANUFACTURER: u64 = 3;
    pub const MODEL: u64 = 4;
    pub const NAME: u64 = 5;
    pub const SERIAL: u64 = 6;
    pub const FIRMWARE: u64 = 7;
    pub const PROTOCOL_VERSION: u64 = 9;
    pub const ON: u64 = 9;
    pub const BRIGHTNESS: u64 = 10;
    pub const HUE: u64 = 11;
    pub const SATURATION: u64 = 12;
    pub const COLOR_TEMPERATURE: u64 = 13;
}

pub mod status {
    pub const OK: i32 = 0;
    pub const COMMUNICATION: i32 = -70402;
    pub const READ_ONLY: i32 = -70404;
    pub const WRITE_ONLY: i32 = -70405;
    pub const NOT_FOUND: i32 = -70409;
    pub const INVALID: i32 = -70410;
}

pub struct BridgeInfo<'a> {
    pub name: &'a str,
    pub serial: &'a str,
}

pub fn database(
    info: &BridgeInfo<'_>,
    lights: &BTreeMap<DeviceId, StoredLight>,
    hub: &Hub,
) -> Json {
    let mut accessories = vec![json!({
        "aid": BRIDGE_AID,
        "services": [
            information(info.name, "Hestia", "Bridge", info.serial),
            {"iid": 8, "type": "A2", "characteristics": [
                {"iid": iid::PROTOCOL_VERSION, "type": "37", "perms": ["pr"], "format": "string", "value": "1.1.0"}
            ]}
        ]
    })];
    for (device, light) in lights {
        let values = LightValues::of(light, hub.device(device).as_ref());
        let shape = &light.shape;
        let dynamic = ["pr", "pw", "ev"];
        let mut characteristics = vec![
            json!({"iid": iid::ON, "type": "25", "perms": dynamic, "format": "bool", "value": values.on}),
            json!({"iid": iid::BRIGHTNESS, "type": "8", "perms": dynamic, "format": "int", "unit": "percentage",
                   "minValue": 0, "maxValue": 100, "minStep": 1, "value": values.brightness}),
        ];
        if shape.color.is_some() {
            characteristics.push(json!({"iid": iid::HUE, "type": "13", "perms": dynamic, "format": "float",
                "unit": "arcdegrees", "minValue": 0, "maxValue": 360, "minStep": 1, "value": values.hue}));
            characteristics.push(json!({"iid": iid::SATURATION, "type": "2F", "perms": dynamic, "format": "float",
                "unit": "percentage", "minValue": 0, "maxValue": 100, "minStep": 1, "value": values.saturation}));
        }
        if let Some(temperature) = &shape.temperature {
            characteristics.push(json!({"iid": iid::COLOR_TEMPERATURE, "type": "CE", "perms": dynamic,
                "format": "uint32", "minValue": kelvin_to_mired(temperature.max_kelvin),
                "maxValue": kelvin_to_mired(temperature.min_kelvin), "minStep": 1, "value": values.mired}));
        }
        accessories.push(json!({
            "aid": light.aid,
            "services": [
                information(
                    &shape.name,
                    shape.manufacturer.as_deref().unwrap_or("Hestia"),
                    shape.model.as_deref().unwrap_or("Light"),
                    device.as_str(),
                ),
                {"iid": 8, "type": "43", "primary": true, "characteristics": characteristics}
            ]
        }));
    }
    json!({ "accessories": accessories })
}

fn information(name: &str, manufacturer: &str, model: &str, serial: &str) -> Json {
    json!({"iid": 1, "type": "3E", "characteristics": [
        {"iid": iid::IDENTIFY, "type": "14", "perms": ["pw"], "format": "bool"},
        {"iid": iid::MANUFACTURER, "type": "20", "perms": ["pr"], "format": "string", "value": manufacturer},
        {"iid": iid::MODEL, "type": "21", "perms": ["pr"], "format": "string", "value": model},
        {"iid": iid::NAME, "type": "23", "perms": ["pr"], "format": "string", "value": name},
        {"iid": iid::SERIAL, "type": "30", "perms": ["pr"], "format": "string", "value": serial},
        {"iid": iid::FIRMWARE, "type": "52", "perms": ["pr"], "format": "string", "value": FIRMWARE},
    ]})
}

/// Reads one characteristic, or the HAP status explaining why it cannot be read.
pub fn read(
    info: &BridgeInfo<'_>,
    lights: &BTreeMap<DeviceId, StoredLight>,
    hub: &Hub,
    aid: u64,
    iid: u64,
) -> Result<Json, i32> {
    if aid == BRIDGE_AID {
        return match iid {
            iid::IDENTIFY => Err(status::WRITE_ONLY),
            iid::MANUFACTURER => Ok(json!("Hestia")),
            iid::MODEL => Ok(json!("Bridge")),
            iid::NAME => Ok(json!(info.name)),
            iid::SERIAL => Ok(json!(info.serial)),
            iid::FIRMWARE => Ok(json!(FIRMWARE)),
            iid::PROTOCOL_VERSION => Ok(json!("1.1.0")),
            _ => Err(status::NOT_FOUND),
        };
    }
    let (device, light) = find(lights, aid).ok_or(status::NOT_FOUND)?;
    let shape = &light.shape;
    match iid {
        iid::IDENTIFY => return Err(status::WRITE_ONLY),
        iid::MANUFACTURER => {
            return Ok(json!(shape.manufacturer.as_deref().unwrap_or("Hestia")));
        }
        iid::MODEL => return Ok(json!(shape.model.as_deref().unwrap_or("Light"))),
        iid::NAME => return Ok(json!(shape.name)),
        iid::SERIAL => return Ok(json!(device.as_str())),
        iid::FIRMWARE => return Ok(json!(FIRMWARE)),
        _ => {}
    }
    let snapshot = hub
        .device(device)
        .filter(|snapshot| snapshot.available)
        .ok_or(status::COMMUNICATION)?;
    let values = LightValues::of(light, Some(&snapshot));
    match iid {
        iid::ON => Ok(json!(values.on)),
        iid::BRIGHTNESS => Ok(json!(values.brightness)),
        iid::HUE if shape.color.is_some() => Ok(json!(values.hue)),
        iid::SATURATION if shape.color.is_some() => Ok(json!(values.saturation)),
        iid::COLOR_TEMPERATURE if shape.temperature.is_some() => Ok(json!(values.mired)),
        _ => Err(status::NOT_FOUND),
    }
}

/// Characteristics whose value may change when `point` changes on `device`.
pub fn affected(
    lights: &BTreeMap<DeviceId, StoredLight>,
    device: &DeviceId,
    point: &str,
) -> Vec<(u64, u64)> {
    let Some(light) = lights.get(device) else {
        return Vec::new();
    };
    let shape = &light.shape;
    let color = shape.color.is_some();
    let mut iids = Vec::new();
    if point == shape.on {
        iids.push(iid::ON);
    } else if point == shape.brightness {
        iids.push(iid::BRIGHTNESS);
    } else if shape.color.as_deref() == Some(point) {
        iids.extend([iid::HUE, iid::SATURATION]);
    } else if shape
        .temperature
        .as_ref()
        .is_some_and(|temperature| temperature.key == point)
    {
        iids.push(iid::COLOR_TEMPERATURE);
        if color {
            iids.extend([iid::HUE, iid::SATURATION]);
        }
    } else if color {
        // Mode switches (colour ↔ white) change the hue shown.
        iids.extend([iid::HUE, iid::SATURATION]);
    }
    iids.into_iter().map(|iid| (light.aid, iid)).collect()
}

pub fn find(
    lights: &BTreeMap<DeviceId, StoredLight>,
    aid: u64,
) -> Option<(&DeviceId, &StoredLight)> {
    lights.iter().find(|(_, light)| light.aid == aid)
}

struct LightValues {
    on: bool,
    brightness: i64,
    hue: f64,
    saturation: f64,
    mired: f64,
}

impl LightValues {
    fn of(light: &StoredLight, snapshot: Option<&DeviceSnapshot>) -> Self {
        let shape = &light.shape;
        let value = |key: &str| {
            snapshot
                .and_then(|snapshot| snapshot.points.iter().find(|point| point.spec.key == key))
                .and_then(|point| point.value.clone())
        };
        let white_mode = snapshot
            .and_then(|snapshot| {
                snapshot
                    .points
                    .iter()
                    .find(|point| point.spec.semantic == Semantic::ColorMode)
            })
            .and_then(|point| point.value.as_ref())
            .is_some_and(|mode| mode.as_text() == Some("white"));
        let kelvin = shape
            .temperature
            .as_ref()
            .and_then(|temperature| value(&temperature.key))
            .and_then(|kelvin| kelvin.as_number());
        let rgb = shape
            .color
            .as_deref()
            .and_then(value)
            .and_then(|color| color.as_color());
        let (hue, saturation) = match (white_mode, kelvin, rgb) {
            (true, Some(kelvin), _) => rgb_to_hs(kelvin_to_rgb(kelvin)),
            (_, _, Some(rgb)) => rgb_to_hs(rgb),
            _ => (0.0, 0.0),
        };
        #[allow(clippy::cast_possible_truncation)]
        let brightness = value(&shape.brightness)
            .and_then(|level| level.as_number())
            .map_or(0, |level| level.round().clamp(0.0, 100.0) as i64);
        Self {
            on: value(&shape.on)
                .and_then(|on| on.as_bool())
                .unwrap_or(false),
            brightness,
            hue,
            saturation,
            mired: kelvin_to_mired(kelvin.unwrap_or(4000.0)),
        }
    }
}

/// Applies the writes aimed at one light and returns a status per instance id.
pub async fn write(
    device: &DeviceId,
    light: &StoredLight,
    hub: &Hub,
    changes: &[(u64, Json)],
) -> Vec<(u64, i32)> {
    let shape = &light.shape;
    let mut statuses: Vec<(u64, i32)> = Vec::new();
    let mut on = None;
    let mut brightness = None;
    let mut hue = None;
    let mut saturation = None;
    let mut mired = None;
    for (iid, value) in changes {
        let parsed = match *iid {
            iid::IDENTIFY => Ok(()),
            iid::MANUFACTURER | iid::MODEL | iid::NAME | iid::SERIAL | iid::FIRMWARE => {
                Err(status::READ_ONLY)
            }
            iid::ON => as_bool(value).map(|value| on = Some(value)),
            iid::BRIGHTNESS => as_number(value, 0.0, 100.0).map(|value| brightness = Some(value)),
            iid::HUE if shape.color.is_some() => {
                as_number(value, 0.0, 360.0).map(|value| hue = Some(value))
            }
            iid::SATURATION if shape.color.is_some() => {
                as_number(value, 0.0, 100.0).map(|value| saturation = Some(value))
            }
            iid::COLOR_TEMPERATURE if shape.temperature.is_some() => {
                as_number(value, 50.0, 1000.0).map(|value| mired = Some(value))
            }
            _ => Err(status::NOT_FOUND),
        };
        statuses.push((*iid, parsed.err().unwrap_or(status::OK)));
    }

    let current = LightValues::of(light, hub.device(device).as_ref());
    let mut commands: Vec<(Vec<u64>, String, Value)> = Vec::new();
    if on == Some(true) {
        commands.push((vec![iid::ON], shape.on.clone(), Value::Bool(true)));
    }
    if let Some(level) = brightness {
        commands.push((
            vec![iid::BRIGHTNESS],
            shape.brightness.clone(),
            Value::Number(level.round()),
        ));
    }
    if let (Some(key), true) = (&shape.color, hue.is_some() || saturation.is_some()) {
        let rgb = hs_to_rgb(
            hue.unwrap_or(current.hue),
            saturation.unwrap_or(current.saturation),
        );
        commands.push((
            vec![iid::HUE, iid::SATURATION],
            key.clone(),
            Value::Color(rgb),
        ));
    }
    if let (Some(temperature), Some(mired)) = (&shape.temperature, mired) {
        let kelvin = mired_to_kelvin(mired).clamp(temperature.min_kelvin, temperature.max_kelvin);
        commands.push((
            vec![iid::COLOR_TEMPERATURE],
            temperature.key.clone(),
            Value::Number(kelvin),
        ));
    }
    if on == Some(false) {
        commands.push((vec![iid::ON], shape.on.clone(), Value::Bool(false)));
    }

    for (iids, point, value) in commands {
        let command = Command {
            device: device.clone(),
            point,
            value,
        };
        if let Err(error) = hub.command(command, Origin::HomeKit).await {
            debug!(device = %device, %error, "HomeKit write failed");
            for (iid, code) in &mut statuses {
                if iids.contains(iid) {
                    *code = status::COMMUNICATION;
                }
            }
        }
    }
    statuses
}

fn as_bool(value: &Json) -> Result<bool, i32> {
    match value {
        Json::Bool(flag) => Ok(*flag),
        Json::Number(number) => Ok(number.as_f64().is_some_and(|number| number != 0.0)),
        _ => Err(status::INVALID),
    }
}

fn as_number(value: &Json, min: f64, max: f64) -> Result<f64, i32> {
    value
        .as_f64()
        .filter(|number| number.is_finite() && (min..=max).contains(number))
        .ok_or(status::INVALID)
}
