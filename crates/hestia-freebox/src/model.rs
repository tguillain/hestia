//! Shapes of the Freebox answers used by the driver.

use hestia_core::{PointKind, PointSpec, Semantic, Unit};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    pub state: String,
    #[serde(default)]
    pub ipv4: String,
    /// Bytes per second.
    #[serde(default)]
    pub rate_down: u64,
    #[serde(default)]
    pub rate_up: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WifiConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct System {
    #[serde(default)]
    pub firmware_version: String,
    #[serde(default)]
    pub sensors: Vec<Sensor>,
    #[serde(default)]
    pub fans: Vec<Sensor>,
    // Older boxes expose fixed fields instead of the lists.
    #[serde(default)]
    pub temp_cpum: Option<f64>,
    #[serde(default)]
    pub temp_cpub: Option<f64>,
    #[serde(default)]
    pub temp_sw: Option<f64>,
    #[serde(default)]
    pub fan_rpm: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sensor {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub fan: bool,
}

impl Reading {
    pub fn spec(&self) -> PointSpec {
        if self.fan {
            PointSpec::new(
                &self.key,
                &self.label,
                PointKind::Numeric {
                    min: 0.0,
                    max: 50_000.0,
                    step: 1.0,
                },
                Semantic::Other,
            )
            .with_unit(Unit::RevolutionPerMinute)
        } else {
            PointSpec::new(
                &self.key,
                &self.label,
                PointKind::Numeric {
                    min: -40.0,
                    max: 150.0,
                    step: 0.1,
                },
                Semantic::Temperature,
            )
            .with_unit(Unit::Celsius)
        }
    }
}

impl System {
    /// Temperatures and fan speeds, whatever the box generation.
    pub fn readings(&self) -> Vec<Reading> {
        let mut readings = Vec::new();
        let mut push = |prefix: &str, id: &str, name: &str, value: Option<f64>, fan: bool| {
            if let Some(value) = value {
                readings.push(Reading {
                    key: format!("{prefix}_{}", sanitize(id)),
                    label: if name.is_empty() {
                        id.to_owned()
                    } else {
                        name.to_owned()
                    },
                    value,
                    fan,
                });
            }
        };
        for sensor in &self.sensors {
            push("temp", &sensor.id, &sensor.name, sensor.value, false);
        }
        for fan in &self.fans {
            push("fan", &fan.id, &fan.name, fan.value, true);
        }
        if self.sensors.is_empty() {
            push("temp", "cpum", "Température CPU M", self.temp_cpum, false);
            push("temp", "cpub", "Température CPU B", self.temp_cpub, false);
            push("temp", "sw", "Température switch", self.temp_sw, false);
        }
        if self.fans.is_empty() {
            push("fan", "rpm", "Ventilateur", self.fan_rpm, true);
        }
        readings
    }
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::System;

    #[test]
    fn modern_and_legacy_sensors() {
        let modern: System = serde_json::from_value(serde_json::json!({
            "firmware_version": "4.13.1",
            "sensors": [{"id": "temp_cpu0", "name": "Température CPU 0", "value": 56}],
            "fans": [{"id": "fan0_speed", "name": "Ventilateur 1", "value": 1840}]
        }))
        .unwrap();
        let readings = modern.readings();
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].key, "temp_temp_cpu0");
        assert!(readings[1].fan);

        let legacy: System =
            serde_json::from_value(serde_json::json!({"temp_cpum": 61, "fan_rpm": 1200})).unwrap();
        let keys: Vec<_> = legacy
            .readings()
            .into_iter()
            .map(|reading| reading.key)
            .collect();
        assert_eq!(keys, ["temp_cpum", "fan_rpm"]);
    }
}
