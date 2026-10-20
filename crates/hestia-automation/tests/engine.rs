//! Rules running against a real hub and a fake driver.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use hestia_automation::{Action, Automations, Document, PresenceEvent, Rule, SetPoint, Trigger};
use hestia_core::{Command, DeviceId, DeviceInfo, Origin, PointKind, PointSpec, Semantic, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext, Hub, spawn_driver};

/// A lamp and a person whose presence the test sets like a command.
struct House;

impl Driver for House {
    fn kind(&self) -> &'static str {
        "house"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let lamp = ctx.device_id("lamp")?;
            let person = ctx.device_id("thomas")?;
            ctx.upsert_device(DeviceInfo {
                id: lamp.clone(),
                name: "Lampe".to_owned(),
                manufacturer: None,
                model: None,
                points: vec![
                    PointSpec::new("on", "Allumage", PointKind::Binary, Semantic::OnOff).writable(),
                ],
            });
            ctx.upsert_device(DeviceInfo {
                id: person.clone(),
                name: "Thomas".to_owned(),
                manufacturer: None,
                model: None,
                points: vec![
                    PointSpec::new("home", "À la maison", PointKind::Binary, Semantic::Presence)
                        .writable(),
                ],
            });
            ctx.set_state(&lamp, "on", Value::Bool(false));
            ctx.set_state(&person, "home", Value::Bool(true));
            ctx.set_available(&lamp, true);
            ctx.set_available(&person, true);
            loop {
                let pending = ctx.next_command().await;
                let command = pending.command().clone();
                ctx.set_state(&command.device, &command.point, command.value);
                pending.succeed();
            }
        })
    }
}

fn id(raw: &str) -> DeviceId {
    raw.parse().unwrap()
}

async fn wait_for(hub: &Hub, device: &DeviceId, point: &str, expected: &Value) {
    for _ in 0..200 {
        if hub.value(device, point).as_ref() == Some(expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{device} {point} never became {expected:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_and_state_rules_command_the_lamp() {
    let hub = Hub::new(HashMap::new());
    spawn_driver(&hub, "house", Arc::new(House));
    let lamp = id("house:lamp");
    let person = id("house:thomas");
    wait_for(&hub, &person, "home", &Value::Bool(true)).await;

    let dir = std::env::temp_dir().join(format!("hestia-automation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let automations = Automations::open(Arc::clone(&hub), &dir).unwrap();
    let lamp_on = SetPoint {
        device: lamp.clone(),
        point: "on".to_owned(),
        value: Value::Bool(true),
    };
    let saved = automations
        .replace(Document {
            location: None,
            scenes: vec![hestia_automation::Scene {
                id: "welcome".to_owned(),
                name: "Bienvenue".to_owned(),
                actions: vec![lamp_on],
            }],
            rules: vec![
                Rule {
                    id: String::new(),
                    name: "Tout éteindre en partant".to_owned(),
                    enabled: true,
                    trigger: Trigger::Presence {
                        event: PresenceEvent::LastDeparture,
                    },
                    action: Action::Set(SetPoint {
                        device: lamp.clone(),
                        point: "on".to_owned(),
                        value: Value::Bool(false),
                    }),
                },
                Rule {
                    id: String::new(),
                    name: "Accueil".to_owned(),
                    enabled: true,
                    trigger: Trigger::Presence {
                        event: PresenceEvent::FirstArrival,
                    },
                    action: Action::Scene {
                        scene: "welcome".to_owned(),
                    },
                },
            ],
        })
        .unwrap();
    assert!(
        saved.rules.iter().all(|rule| !rule.id.is_empty()),
        "ids assigned"
    );
    automations.spawn();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let leave = Command {
        device: person.clone(),
        point: "home".to_owned(),
        value: Value::Bool(false),
    };
    hub.command(
        Command {
            value: Value::Bool(true),
            ..leave.clone()
        },
        Origin::Ui,
    )
    .await
    .unwrap();
    let light = Command {
        device: lamp.clone(),
        point: "on".to_owned(),
        value: Value::Bool(true),
    };
    hub.command(light, Origin::Ui).await.unwrap();
    hub.command(leave.clone(), Origin::Ui).await.unwrap();
    wait_for(&hub, &lamp, "on", &Value::Bool(false)).await;

    hub.command(
        Command {
            value: Value::Bool(true),
            ..leave
        },
        Origin::Ui,
    )
    .await
    .unwrap();
    wait_for(&hub, &lamp, "on", &Value::Bool(true)).await;

    assert!(dir.join("automations.json").exists());
    automations.run_scene("welcome", Origin::Ui).await.unwrap();
    assert!(automations.run_scene("nope", Origin::Ui).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
