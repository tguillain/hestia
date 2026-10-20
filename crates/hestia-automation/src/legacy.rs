//! Rules of the first version (one trigger, one action), read once and turned into graphs.

use hestia_core::{DeviceId, Value};
use serde::Deserialize;

use crate::model::{Automation, Edge, Graph, Mode, Node, PresenceEvent, SetPoint, Step, SunEvent};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LegacyRule {
    pub id: String,
    pub name: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub trigger: LegacyTrigger,
    pub action: LegacyAction,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LegacyTrigger {
    Time {
        at: String,
        #[serde(default)]
        days: Vec<u8>,
    },
    Sun {
        event: SunEvent,
        #[serde(default)]
        offset_min: i32,
        #[serde(default)]
        days: Vec<u8>,
    },
    Presence {
        event: PresenceEvent,
    },
    State {
        device: DeviceId,
        point: String,
        value: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LegacyAction {
    Scene { scene: String },
    Set(SetPoint),
    LightsOff,
}

impl LegacyRule {
    /// The same rule as a two-node graph.
    #[must_use]
    pub fn into_automation(self) -> Automation {
        let trigger = match self.trigger {
            LegacyTrigger::Time { at, days } => Step::AtTime { at, days },
            LegacyTrigger::Sun {
                event,
                offset_min,
                days,
            } => Step::AtSun {
                event,
                offset_min,
                days,
            },
            LegacyTrigger::Presence { event } => Step::Presence { event },
            LegacyTrigger::State {
                device,
                point,
                value,
            } => Step::WhenState {
                device,
                point,
                to: Some(value),
                from: None,
                for_s: 0,
            },
        };
        let action = match self.action {
            LegacyAction::Scene { scene } => Step::Scene { scene },
            LegacyAction::Set(SetPoint {
                device,
                point,
                value,
            }) => Step::Set {
                device,
                point,
                value,
            },
            LegacyAction::LightsOff => Step::LightsOff,
        };
        Automation {
            id: self.id,
            name: self.name,
            enabled: self.enabled,
            mode: Mode::Restart,
            graph: Graph {
                nodes: vec![
                    Node {
                        id: "t".to_owned(),
                        step: trigger,
                        x: 0.0,
                        y: 0.0,
                    },
                    Node {
                        id: "a".to_owned(),
                        step: action,
                        x: 300.0,
                        y: 0.0,
                    },
                ],
                edges: vec![Edge {
                    from: "t".to_owned(),
                    port: "out".to_owned(),
                    to: "a".to_owned(),
                }],
            },
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{Document, Step};

    #[test]
    fn first_version_rules_become_graphs() {
        let document: Document = serde_json::from_value(serde_json::json!({
            "rules": [{"id": "r1", "name": "Le soir", "trigger": {"type": "sun", "event": "sunset", "offset_min": -15},
                       "action": {"type": "lights_off"}}]
        }))
        .unwrap();
        let automation = document.rules[0].clone().into_automation();
        assert!(automation.enabled);
        assert!(matches!(
            automation.graph.nodes[0].step,
            Step::AtSun {
                offset_min: -15,
                ..
            }
        ));
        assert_eq!(automation.graph.nodes[1].step, Step::LightsOff);
        assert_eq!(automation.graph.edges.len(), 1);
    }
}
