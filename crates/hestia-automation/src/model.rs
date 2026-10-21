//! What the household sets up, kept in `<data>/automations.json`.
//!
//! An automation is a graph, n8n style: a trigger node, then steps linked by wires. A wire
//! leaves a node by one of its ports (`out`, or `yes`/`no` after a condition) and enters
//! the next node.

use std::collections::BTreeMap;

use hestia_core::{DeviceId, Value};
use serde::{Deserialize, Serialize};

use crate::legacy::LegacyRule;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Where the house is, for sunrise and sunset.
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub scenes: Vec<Scene>,
    #[serde(default)]
    pub automations: Vec<Automation>,
    /// Rules of the first version, turned into automations when read.
    #[serde(default, skip_serializing)]
    pub rules: Vec<LegacyRule>,
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
pub struct Automation {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: Mode,
    pub graph: Graph,
    /// Why Hestia switched it off, for instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What a trigger does while the automation is still running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Stop the run in progress and start again.
    #[default]
    Restart,
    /// Ignore the trigger.
    Single,
    /// Run afterwards.
    Queued,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(flatten)]
    pub step: Step,
    /// Position on the canvas only.
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    #[serde(default = "out")]
    pub port: String,
    pub to: String,
}

fn out() -> String {
    "out".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    // ---- Triggers
    /// A point changes, optionally to or from a value, and stays so for `for_s` seconds.
    WhenState {
        device: DeviceId,
        point: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Value>,
        #[serde(default)]
        for_s: u32,
    },
    /// A number crosses a threshold.
    WhenThreshold {
        device: DeviceId,
        point: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        above: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        below: Option<f64>,
        #[serde(default)]
        for_s: u32,
    },
    /// `HH:MM` on the listed days (1 = Monday … 7 = Sunday; none = every day).
    AtTime {
        at: String,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// Sunrise or sunset, shifted by `offset_min` (negative: before).
    AtSun {
        event: SunEvent,
        #[serde(default)]
        offset_min: i32,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// Every `minutes`, counted from midnight.
    Every {
        minutes: u32,
    },
    /// Shortly after Hestia starts.
    OnStart,
    /// Only when someone presses « Lancer ».
    Manual,
    /// The first person arriving, or the last one leaving.
    Presence {
        event: PresenceEvent,
    },

    // ---- Logic
    /// Leaves by `yes` or `no`.
    If {
        rules: Vec<Rule>,
        /// All rules (and) or any of them (or).
        #[serde(default = "yes")]
        all: bool,
    },
    Wait {
        seconds: u32,
    },
    /// Leaves by `ok` as soon as the rule holds, or by `timeout`.
    WaitFor {
        rule: Rule,
        timeout_s: u32,
    },

    // ---- Actions
    /// The device as it should be afterwards: each listed point is set (a switch turned on
    /// first, turned off last), points left out keep their state. `toggle` inverts an
    /// on/off point instead.
    Device {
        device: DeviceId,
        #[serde(default)]
        values: BTreeMap<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        toggle: Option<String>,
    },
    /// One point, as the first version of the editor wrote it.
    Set {
        device: DeviceId,
        point: String,
        value: Value,
    },
    Toggle {
        device: DeviceId,
        point: String,
    },
    Scene {
        scene: String,
    },
    /// Every lamp (a switch with brightness or colour), never other switches such as Wi-Fi.
    LightsOff,
}

fn yes() -> bool {
    true
}

impl Step {
    #[must_use]
    pub fn is_trigger(&self) -> bool {
        matches!(
            self,
            Self::WhenState { .. }
                | Self::WhenThreshold { .. }
                | Self::AtTime { .. }
                | Self::AtSun { .. }
                | Self::Every { .. }
                | Self::OnStart
                | Self::Manual
                | Self::Presence { .. }
        )
    }

    /// The ports a wire may leave by.
    #[must_use]
    pub fn ports(&self) -> &'static [&'static str] {
        match self {
            Self::If { .. } => &["yes", "no"],
            Self::WaitFor { .. } => &["ok", "timeout"],
            _ => &["out"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    State {
        device: DeviceId,
        point: String,
        #[serde(default)]
        op: Op,
        value: Value,
    },
    /// Between `after` and `before` (`HH:MM`, either may be missing, may wrap midnight).
    Time {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
        #[serde(default)]
        days: Vec<u8>,
    },
    Sun {
        is: Daylight,
    },
    Presence {
        is: Occupancy,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    #[default]
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Daylight {
    Day,
    Night,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occupancy {
    Someone,
    Nobody,
}

impl Graph {
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The nodes a wire from `from`'s `port` leads to, in wire order.
    pub fn next<'a>(&'a self, from: &'a str, port: &'a str) -> impl Iterator<Item = &'a Node> {
        self.edges
            .iter()
            .filter(move |edge| edge.from == from && edge.port == port)
            .filter_map(|edge| self.node(&edge.to))
    }

    pub fn triggers(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|node| node.step.is_trigger())
    }
}

impl Document {
    #[must_use]
    pub fn scene(&self, id: &str) -> Option<&Scene> {
        self.scenes.iter().find(|scene| scene.id == id)
    }

    #[must_use]
    pub fn automation(&self, id: &str) -> Option<&Automation> {
        self.automations
            .iter()
            .find(|automation| automation.id == id)
    }
}

/// `HH:MM` as minutes after midnight.
#[must_use]
pub fn parse_time(raw: &str) -> Option<u32> {
    let (hours, minutes) = raw.trim().split_once(':')?;
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

#[cfg(test)]
mod tests {
    use super::{Document, Step, parse_time};

    #[test]
    fn times() {
        assert_eq!(parse_time("07:30"), Some(450));
        assert_eq!(parse_time("7:05"), Some(425));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("midi"), None);
    }

    #[test]
    fn graphs_read_like_moli() {
        let document: Document = serde_json::from_value(serde_json::json!({
            "automations": [{
                "id": "portail", "name": "Portail ouvert la nuit", "enabled": true,
                "graph": {
                    "nodes": [
                        {"id": "t", "type": "when_state", "device": "z:portail", "point": "contact", "to": false, "x": 0, "y": 0},
                        {"id": "n1", "type": "if", "rules": [{"kind": "sun", "is": "night"}], "x": 300, "y": 0},
                        {"id": "n2", "type": "set", "device": "hue:entree", "point": "on", "value": true},
                        {"id": "n3", "type": "wait", "seconds": 300},
                        {"id": "n4", "type": "set", "device": "hue:entree", "point": "on", "value": false}
                    ],
                    "edges": [
                        {"from": "t", "to": "n1"},
                        {"from": "n1", "port": "yes", "to": "n2"},
                        {"from": "n2", "to": "n3"},
                        {"from": "n3", "to": "n4"}
                    ]
                }
            }]
        }))
        .unwrap();
        let graph = &document.automations[0].graph;
        assert!(graph.nodes[0].step.is_trigger());
        assert!(matches!(graph.nodes[1].step, Step::If { all: true, .. }));
        assert_eq!(
            graph
                .next("n1", "yes")
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            ["n2"]
        );
        assert_eq!(graph.next("n1", "no").count(), 0);
        assert_eq!(graph.edges[0].port, "out");
        let back = serde_json::to_value(&document).unwrap();
        assert_eq!(
            back["automations"][0]["graph"]["nodes"][0]["type"],
            "when_state"
        );
        assert!(
            back.get("rules").is_none(),
            "legacy rules are never written back"
        );
    }
}
