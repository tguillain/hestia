//! A scene is recorded from what the lamps show right now.

use hestia_core::{Semantic, Value};
use hestia_hub::DeviceSnapshot;

use crate::model::SetPoint;

/// The writes that bring `device` back to its current state: an unlit lamp only needs
/// turning off; a lit one gets its brightness and the colour of its active mode.
#[must_use]
pub fn capture(device: &DeviceSnapshot) -> Vec<SetPoint> {
    let current = |semantic: Semantic| {
        device
            .points
            .iter()
            .find(|point| point.spec.semantic == semantic && point.spec.is_writable())
            .and_then(|point| {
                point
                    .value
                    .clone()
                    .map(|value| (point.spec.key.clone(), value))
            })
    };
    let set = |(point, value): (String, Value)| SetPoint {
        device: device.id.clone(),
        point,
        value,
    };
    let Some(on) = current(Semantic::OnOff) else {
        return Vec::new();
    };
    if on.1 == Value::Bool(false) {
        return vec![set(on)];
    }
    let mode = device
        .points
        .iter()
        .find(|point| point.spec.semantic == Semantic::ColorMode)
        .and_then(|point| {
            point
                .value
                .as_ref()
                .and_then(Value::as_text)
                .map(str::to_owned)
        });
    let mut actions = vec![set(on)];
    actions.extend(current(Semantic::Brightness).map(set));
    let shade = match mode.as_deref() {
        Some("white") => current(Semantic::ColorTemperature),
        Some(_) => current(Semantic::Color),
        None => current(Semantic::Color).or_else(|| current(Semantic::ColorTemperature)),
    };
    actions.extend(shade.map(set));
    actions
}

#[cfg(test)]
mod tests {
    use super::capture;
    use hestia_core::{PointKind, PointSpec, Rgb, Semantic, Value};
    use hestia_hub::{DeviceSnapshot, PointSnapshot};

    fn lamp(on: bool, mode: &str) -> DeviceSnapshot {
        let point = |spec: PointSpec, value: Value| PointSnapshot {
            spec,
            value: Some(value),
            at: None,
        };
        let numeric = PointKind::Numeric {
            min: 0.0,
            max: 9000.0,
            step: 1.0,
        };
        DeviceSnapshot {
            id: "govee:lamp".parse().unwrap(),
            name: "Lampe".to_owned(),
            room: None,
            manufacturer: None,
            model: None,
            available: true,
            points: vec![
                point(
                    PointSpec::new("on", "", PointKind::Binary, Semantic::OnOff).writable(),
                    Value::Bool(on),
                ),
                point(
                    PointSpec::new("brightness", "", numeric.clone(), Semantic::Brightness)
                        .writable(),
                    Value::Number(40.0),
                ),
                point(
                    PointSpec::new("color", "", PointKind::Color, Semantic::Color).writable(),
                    Value::Color(Rgb { r: 255, g: 0, b: 0 }),
                ),
                point(
                    PointSpec::new("color_temp", "", numeric, Semantic::ColorTemperature)
                        .writable(),
                    Value::Number(2700.0),
                ),
                point(
                    PointSpec::new(
                        "color_mode",
                        "",
                        PointKind::Enum { values: vec![] },
                        Semantic::ColorMode,
                    ),
                    Value::Text(mode.to_owned()),
                ),
            ],
        }
    }

    #[test]
    fn records_the_active_mode_only() {
        let points = |device| {
            capture(&device)
                .into_iter()
                .map(|set| set.point)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            points(lamp(true, "white")),
            ["on", "brightness", "color_temp"]
        );
        assert_eq!(points(lamp(true, "color")), ["on", "brightness", "color"]);
        assert_eq!(points(lamp(false, "color")), ["on"]);
    }
}
