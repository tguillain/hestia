//! Translation between Matter light clusters and the hub's points. No I/O.

use hestia_core::color::{hs_to_rgb, rgb_to_hs, rgb_to_xy, xy_to_rgb};
use hestia_core::{DeviceId, DeviceInfo, PointKind, PointSpec, Semantic, Unit, Value};

use crate::store::{Features, PairedDevice};

pub const ON_OFF: u32 = 0x0006;
pub const LEVEL_CONTROL: u32 = 0x0008;
pub const COLOR_CONTROL: u32 = 0x0300;

pub const ON_OFF_ON: u32 = 0x0000;
pub const LEVEL_CURRENT: u32 = 0x0000;
pub const COLOR_HUE: u32 = 0x0000;
pub const COLOR_SATURATION: u32 = 0x0001;
pub const COLOR_X: u32 = 0x0003;
pub const COLOR_Y: u32 = 0x0004;
pub const COLOR_MIREDS: u32 = 0x0007;
pub const COLOR_MODE: u32 = 0x0008;
pub const COLOR_CAPABILITIES: u32 = 0x400a;
pub const COLOR_MIREDS_MIN: u32 = 0x400b;
pub const COLOR_MIREDS_MAX: u32 = 0x400c;

/// `ColorCapabilities` bits.
pub const CAPABILITY_HUE_SATURATION: u16 = 1 << 0;
pub const CAPABILITY_XY: u16 = 1 << 3;
pub const CAPABILITY_TEMPERATURE: u16 = 1 << 4;

/// Matter levels and hue/saturation go up to 254; 255 is reserved.
const MAX_LEVEL: f64 = 254.0;
const XY_SCALE: f64 = 65_536.0;

/// Attribute values as read; `None` when the device did not report one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LightState {
    pub on: Option<bool>,
    pub level: Option<u8>,
    pub hue: Option<u8>,
    pub saturation: Option<u8>,
    pub x: Option<u16>,
    pub y: Option<u16>,
    pub mireds: Option<u16>,
    pub color_mode: Option<u8>,
}

/// One command sent to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    On,
    Off,
    Level(u8),
    HueSaturation { hue: u8, saturation: u8 },
    Xy { x: u16, y: u16 },
    Mireds(u16),
}

/// The attributes worth reading for a light with these features, as (cluster, attribute).
#[must_use]
pub fn state_attributes(features: &Features) -> Vec<(u32, u32)> {
    let mut attributes = vec![(ON_OFF, ON_OFF_ON)];
    if features.level {
        attributes.push((LEVEL_CONTROL, LEVEL_CURRENT));
    }
    if features.hue_saturation {
        attributes.extend([
            (COLOR_CONTROL, COLOR_HUE),
            (COLOR_CONTROL, COLOR_SATURATION),
        ]);
    }
    if features.xy {
        attributes.extend([(COLOR_CONTROL, COLOR_X), (COLOR_CONTROL, COLOR_Y)]);
    }
    if features.mireds.is_some() {
        attributes.push((COLOR_CONTROL, COLOR_MIREDS));
    }
    if features.hue_saturation || features.xy || features.mireds.is_some() {
        attributes.push((COLOR_CONTROL, COLOR_MODE));
    }
    attributes
}

#[must_use]
pub fn device_info(id: DeviceId, device: &PairedDevice) -> DeviceInfo {
    let features = &device.features;
    let mut points =
        vec![PointSpec::new("on", "Allumage", PointKind::Binary, Semantic::OnOff).writable()];
    if features.level {
        points.push(
            PointSpec::new(
                "brightness",
                "Luminosité",
                PointKind::Numeric {
                    min: 0.0,
                    max: 100.0,
                    step: 1.0,
                },
                Semantic::Brightness,
            )
            .writable()
            .with_unit(Unit::Percent),
        );
    }
    let has_color = features.hue_saturation || features.xy;
    if has_color {
        points
            .push(PointSpec::new("color", "Couleur", PointKind::Color, Semantic::Color).writable());
    }
    if let Some((min_mireds, max_mireds)) = features.mireds {
        points.push(
            PointSpec::new(
                "color_temp",
                "Température de blanc",
                PointKind::Numeric {
                    min: round_kelvin(kelvin(max_mireds)),
                    max: round_kelvin(kelvin(min_mireds)),
                    step: 100.0,
                },
                Semantic::ColorTemperature,
            )
            .writable()
            .with_unit(Unit::Kelvin),
        );
    }
    if has_color && features.mireds.is_some() {
        points.push(PointSpec::new(
            "color_mode",
            "Mode",
            PointKind::Enum {
                values: vec!["color".to_owned(), "white".to_owned()],
            },
            Semantic::ColorMode,
        ));
    }
    let name = match (device.vendor.is_empty(), device.product.is_empty()) {
        (false, false) => format!("{} {}", device.vendor, device.product),
        (false, true) => device.vendor.clone(),
        (true, false) => device.product.clone(),
        (true, true) => "Lumière Matter".to_owned(),
    };
    DeviceInfo {
        id,
        name,
        manufacturer: Some(device.vendor.clone()).filter(|vendor| !vendor.is_empty()),
        model: Some(device.product.clone()).filter(|product| !product.is_empty()),
        points,
    }
}

/// Point values for a state read. The colour reported matches the active colour mode.
#[must_use]
pub fn state_values(state: &LightState, features: &Features) -> Vec<(&'static str, Value)> {
    let mut values = Vec::new();
    if let Some(on) = state.on {
        values.push(("on", Value::Bool(on)));
    }
    if let Some(level) = state.level {
        values.push(("brightness", Value::Number(percent_of_level(level))));
    }
    let white = state.color_mode == Some(2);
    let has_color = features.hue_saturation || features.xy;
    if let Some(mireds) = state.mireds
        && features.mireds.is_some()
        && (white || !has_color)
    {
        values.push(("color_temp", Value::Number(round_kelvin(kelvin(mireds)))));
    }
    if !white {
        let color = match (
            state.color_mode,
            state.hue,
            state.saturation,
            state.x,
            state.y,
        ) {
            (Some(1), _, _, Some(x), Some(y)) | (None, None, _, Some(x), Some(y)) => {
                Some(xy_to_rgb(f64::from(x) / XY_SCALE, f64::from(y) / XY_SCALE))
            }
            (_, Some(hue), Some(saturation), _, _) => Some(hs_to_rgb(
                f64::from(hue) * 360.0 / MAX_LEVEL,
                f64::from(saturation) * 100.0 / MAX_LEVEL,
            )),
            _ => None,
        };
        if let Some(color) = color {
            values.push(("color", Value::Color(color)));
        }
    }
    if has_color
        && features.mireds.is_some()
        && let Some(mode) = state.color_mode
    {
        let mode = if mode == 2 { "white" } else { "color" };
        values.push(("color_mode", Value::Text(mode.to_owned())));
    }
    values
}

/// The command for a point write, or why it cannot be done.
///
/// # Errors
/// Fails when the point is read-only, unknown or unsupported by this light.
pub fn action(point: &str, value: &Value, features: &Features) -> Result<Action, String> {
    match (point, value) {
        ("on", Value::Bool(true)) => Ok(Action::On),
        ("on", Value::Bool(false)) => Ok(Action::Off),
        ("brightness", Value::Number(percent)) if features.level => {
            if *percent < 0.5 {
                Ok(Action::Off)
            } else {
                Ok(Action::Level(level_of_percent(*percent)))
            }
        }
        ("color", Value::Color(rgb)) if features.hue_saturation => {
            let (hue, saturation) = rgb_to_hs(*rgb);
            Ok(Action::HueSaturation {
                hue: scale_to_level(hue / 360.0),
                saturation: scale_to_level(saturation / 100.0),
            })
        }
        ("color", Value::Color(rgb)) if features.xy => {
            let (x, y) = rgb_to_xy(*rgb);
            Ok(Action::Xy {
                x: scale_to_xy(x),
                y: scale_to_xy(y),
            })
        }
        ("color_temp", Value::Number(kelvin)) => match features.mireds {
            Some((min, max)) => Ok(Action::Mireds(mireds_of(*kelvin, min, max))),
            None => Err("this light has no tunable white".to_owned()),
        },
        _ => Err(format!("point {point:?} cannot be written")),
    }
}

fn kelvin(mireds: u16) -> f64 {
    1_000_000.0 / f64::from(mireds.max(1))
}

fn round_kelvin(kelvin: f64) -> f64 {
    (kelvin / 100.0).round() * 100.0
}

fn percent_of_level(level: u8) -> f64 {
    (f64::from(level.min(254)) * 100.0 / MAX_LEVEL).round()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn level_of_percent(percent: f64) -> u8 {
    (percent.clamp(0.0, 100.0) * MAX_LEVEL / 100.0)
        .round()
        .clamp(1.0, MAX_LEVEL) as u8
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scale_to_level(fraction: f64) -> u8 {
    (fraction * MAX_LEVEL).round().clamp(0.0, MAX_LEVEL) as u8
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scale_to_xy(fraction: f64) -> u16 {
    (fraction * XY_SCALE).round().clamp(0.0, 65_279.0) as u16
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn mireds_of(kelvin: f64, min: u16, max: u16) -> u16 {
    (1_000_000.0 / kelvin.max(1.0))
        .round()
        .clamp(f64::from(min), f64::from(max)) as u16
}

#[cfg(test)]
mod tests {
    use super::{Action, Features, LightState, action, device_info, state_values};
    use crate::store::PairedDevice;
    use hestia_core::{Rgb, Value};

    const FULL: Features = Features {
        level: true,
        hue_saturation: true,
        xy: true,
        mireds: Some((153, 500)),
    };

    #[test]
    fn white_mode_reports_temperature_only() {
        let state = LightState {
            on: Some(true),
            level: Some(127),
            hue: Some(10),
            saturation: Some(200),
            mireds: Some(250),
            color_mode: Some(2),
            ..LightState::default()
        };
        let values = state_values(&state, &FULL);
        assert!(values.contains(&("on", Value::Bool(true))));
        assert!(values.contains(&("brightness", Value::Number(50.0))));
        assert!(values.contains(&("color_temp", Value::Number(4000.0))));
        assert!(values.contains(&("color_mode", Value::Text("white".to_owned()))));
        assert!(!values.iter().any(|(point, _)| *point == "color"));
    }

    #[test]
    fn color_mode_follows_hue_or_xy() {
        let hs = LightState {
            hue: Some(0),
            saturation: Some(254),
            color_mode: Some(0),
            ..LightState::default()
        };
        assert!(
            state_values(&hs, &FULL).contains(&("color", Value::Color(Rgb { r: 255, g: 0, b: 0 })))
        );
        let xy = LightState {
            x: Some(45_000),
            y: Some(19_000),
            color_mode: Some(1),
            ..LightState::default()
        };
        let values = state_values(&xy, &FULL);
        let Some((_, Value::Color(color))) = values.iter().find(|(point, _)| *point == "color")
        else {
            panic!("no colour in {values:?}");
        };
        assert!(color.r > 200 && color.g < 80, "{color:?} should be red");
    }

    #[test]
    fn writes_become_commands() {
        assert_eq!(action("on", &Value::Bool(true), &FULL), Ok(Action::On));
        assert_eq!(
            action("brightness", &Value::Number(0.0), &FULL),
            Ok(Action::Off)
        );
        assert_eq!(
            action("brightness", &Value::Number(50.0), &FULL),
            Ok(Action::Level(127))
        );
        assert_eq!(
            action("brightness", &Value::Number(0.6), &FULL),
            Ok(Action::Level(2))
        );
        assert_eq!(
            action("color", &Value::Color(Rgb { r: 0, g: 0, b: 255 }), &FULL),
            Ok(Action::HueSaturation {
                hue: 169,
                saturation: 254
            })
        );
        assert_eq!(
            action("color_temp", &Value::Number(4000.0), &FULL),
            Ok(Action::Mireds(250))
        );
        assert_eq!(
            action("color_temp", &Value::Number(10_000.0), &FULL),
            Ok(Action::Mireds(153))
        );
        let xy_only = Features {
            hue_saturation: false,
            ..FULL
        };
        assert!(matches!(
            action("color", &Value::Color(Rgb { r: 255, g: 0, b: 0 }), &xy_only),
            Ok(Action::Xy { .. })
        ));
        assert!(action("color_mode", &Value::Text("white".to_owned()), &FULL).is_err());
        assert!(action("color_temp", &Value::Number(3000.0), &Features::default()).is_err());
    }

    #[test]
    fn points_follow_features() {
        let device = PairedDevice {
            node_id: 1,
            vendor: "Govee".to_owned(),
            product: "H6047".to_owned(),
            endpoint: 1,
            features: Features {
                mireds: Some((153, 370)),
                ..FULL
            },
            address: None,
        };
        let info = device_info("matter:0000000000000001".parse().unwrap(), &device);
        assert_eq!(info.name, "Govee H6047");
        let keys: Vec<&str> = info.points.iter().map(|point| point.key.as_str()).collect();
        assert_eq!(
            keys,
            ["on", "brightness", "color", "color_temp", "color_mode"]
        );
        let plain = PairedDevice {
            features: Features::default(),
            ..device
        };
        let info = device_info("matter:0000000000000001".parse().unwrap(), &plain);
        assert_eq!(info.points.len(), 1);
    }
}
