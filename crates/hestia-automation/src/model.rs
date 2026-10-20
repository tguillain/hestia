//! What the household sets up in the Settings page, kept in `<data>/automations.json`.

use std::collections::HashSet;

use hestia_core::{DeviceId, Value};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Where the house is, for sunrise and sunset.
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub scenes: Vec<Scene>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}

/// Several points set in one go, e.g. "Soirée".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub id: String,
    pub name: String,
    pub actions: Vec<SetPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetPoint {
    pub device: DeviceId,
    pub point: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub name: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub trigger: Trigger,
    pub action: Action,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// Every listed day (1 = Monday … 7 = Sunday; none = every day) at `at`, `HH:MM`.
    Time {
        at: String,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// Sunrise or sunset, shifted by `offset_min` (negative: before).
    Sun {
        event: SunEvent,
        #[serde(default)]
        offset_min: i32,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// The first person arriving, or the last one leaving.
    Presence { event: PresenceEvent },
    /// A point taking a value, e.g. the desktop computer coming online.
    State {
        device: DeviceId,
        point: String,
        value: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SunEvent {
    Sunrise,
    Sunset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceEvent {
    FirstArrival,
    LastDeparture,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Scene {
        scene: String,
    },
    Set(SetPoint),
    /// Every lamp (a switch with brightness or colour), never other switches such as Wi-Fi.
    LightsOff,
}

/// `HH:MM` as minutes after midnight.
#[must_use]
pub fn parse_time(raw: &str) -> Option<u32> {
    let (hours, minutes) = raw.trim().split_once(':')?;
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

impl Document {
    /// # Errors
    /// The first problem found, worded for the household.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(location) = &self.location
            && !((-90.0..=90.0).contains(&location.latitude)
                && (-180.0..=180.0).contains(&location.longitude))
        {
            return Err("latitude ou longitude hors limites".to_owned());
        }
        let mut ids = HashSet::new();
        for scene in &self.scenes {
            check_name(&scene.name)?;
            if !ids.insert(scene.id.as_str()) {
                return Err(format!("identifiant « {} » en double", scene.id));
            }
        }
        for rule in &self.rules {
            check_name(&rule.name)?;
            if !ids.insert(rule.id.as_str()) {
                return Err(format!("identifiant « {} » en double", rule.id));
            }
            match &rule.trigger {
                Trigger::Time { at, days } => {
                    if parse_time(at).is_none() {
                        return Err(format!("« {at} » n'est pas une heure (HH:MM)"));
                    }
                    check_days(days)?;
                }
                Trigger::Sun {
                    offset_min, days, ..
                } => {
                    if offset_min.abs() > 6 * 60 {
                        return Err("décalage de plus de 6 heures".to_owned());
                    }
                    check_days(days)?;
                }
                Trigger::Presence { .. } | Trigger::State { .. } => {}
            }
            if let Action::Scene { scene } = &rule.action
                && !self.scenes.iter().any(|candidate| &candidate.id == scene)
            {
                return Err(format!(
                    "la règle « {} » lance une scène qui n'existe plus",
                    rule.name
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn scene(&self, id: &str) -> Option<&Scene> {
        self.scenes.iter().find(|scene| scene.id == id)
    }
}

fn check_name(name: &str) -> Result<(), String> {
    let length = name.trim().chars().count();
    if (1..=60).contains(&length) {
        Ok(())
    } else {
        Err("nom de 1 à 60 caractères".to_owned())
    }
}

fn check_days(days: &[u8]) -> Result<(), String> {
    if days.iter().all(|day| (1..=7).contains(day)) {
        Ok(())
    } else {
        Err("jours de 1 (lundi) à 7 (dimanche)".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{Document, parse_time};

    #[test]
    fn times() {
        assert_eq!(parse_time("07:30"), Some(450));
        assert_eq!(parse_time("7:05"), Some(425));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("midi"), None);
    }

    #[test]
    fn documents_are_checked() {
        let document: Document = serde_json::from_value(serde_json::json!({
            "location": {"latitude": 48.85, "longitude": 2.35},
            "scenes": [{"id": "s1", "name": "Soirée", "actions": [
                {"device": "govee:abc", "point": "on", "value": true},
                {"device": "govee:abc", "point": "brightness", "value": 40}
            ]}],
            "rules": [
                {"id": "r1", "name": "Le soir", "trigger": {"type": "sun", "event": "sunset", "offset_min": -15},
                 "action": {"type": "scene", "scene": "s1"}},
                {"id": "r2", "name": "Départ", "trigger": {"type": "presence", "event": "last_departure"},
                 "action": {"type": "set", "device": "govee:abc", "point": "on", "value": false}},
                {"id": "r3", "name": "Réveil", "enabled": false, "trigger": {"type": "time", "at": "07:00", "days": [1, 2, 3, 4, 5]},
                 "action": {"type": "scene", "scene": "s1"}}
            ]
        }))
        .unwrap();
        assert_eq!(document.validate(), Ok(()));
        assert!(document.rules[0].enabled, "enabled by default");

        let mut broken = document.clone();
        broken.scenes.clear();
        assert!(broken.validate().unwrap_err().contains("n'existe plus"));
        let mut duplicate = document;
        duplicate.rules[1].id = "s1".to_owned();
        assert!(duplicate.validate().is_err());
    }
}
