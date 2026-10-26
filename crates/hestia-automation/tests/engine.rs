//! Automations running against a real hub and a fake driver.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use hestia_automation::{Automation, Automations, Document, RunStatus, StepStatus};
use hestia_core::{Command, DeviceId, DeviceInfo, Origin, PointKind, PointSpec, Semantic, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext, Hub, spawn_driver};

/// A lamp, a desk computer and a person, all set like commands.
struct House;

impl Driver for House {
    fn kind(&self) -> &'static str {
        "house"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let devices = [
                (
                    "lamp",
                    "Lampe",
                    PointSpec::new("on", "Allumage", PointKind::Binary, Semantic::OnOff),
                    false,
                ),
                (
                    "pc",
                    "PC",
                    PointSpec::new(
                        "online",
                        "En ligne",
                        PointKind::Binary,
                        Semantic::Connectivity,
                    ),
                    false,
                ),
                (
                    "thomas",
                    "Thomas",
                    PointSpec::new("home", "À la maison", PointKind::Binary, Semantic::Presence),
                    true,
                ),
            ];
            for (native, name, spec, initial) in devices {
                let id = ctx.device_id(native)?;
                let key = spec.key.clone();
                let mut points = vec![spec.writable()];
                if native == "lamp" {
                    // A brightness makes it a lamp for « Éteindre toutes les lumières ».
                    let range = PointKind::Numeric {
                        min: 0.0,
                        max: 100.0,
                        step: 1.0,
                    };
                    points.push(
                        PointSpec::new("brightness", "Luminosité", range, Semantic::Brightness)
                            .writable(),
                    );
                }
                ctx.upsert_device(DeviceInfo {
                    id: id.clone(),
                    name: name.to_owned(),
                    manufacturer: None,
                    model: None,
                    points,
                });
                ctx.set_state(&id, &key, Value::Bool(initial));
                ctx.set_available(&id, true);
            }
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

async fn set(hub: &Hub, device: &str, point: &str, value: bool) {
    let command = Command {
        device: id(device),
        point: point.to_owned(),
        value: Value::Bool(value),
    };
    hub.command(command, Origin::Ui).await.unwrap();
}

async fn wait_for(hub: &Hub, device: &str, point: &str, expected: bool) {
    for _ in 0..300 {
        if hub.value(&id(device), point) == Some(Value::Bool(expected)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{device} {point} never became {expected}");
}

fn automation(value: serde_json::Value) -> Automation {
    serde_json::from_value(value).unwrap()
}

// One house for the whole story: each part depends on the state the previous one left.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graphs_command_the_house() {
    let hub = Hub::new(HashMap::new());
    spawn_driver(&hub, "house", Arc::new(House));
    wait_for(&hub, "house:thomas", "home", true).await;

    let dir = std::env::temp_dir().join(format!("hestia-automation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let automations = Automations::open(Arc::clone(&hub), &dir).unwrap();

    // The desk computer comes online while someone is home: light the lamp.
    let desk = automation(serde_json::json!({
        "name": "Bureau", "enabled": true,
        "graph": {
            "nodes": [
                {"id": "t", "type": "when_state", "device": "house:pc", "point": "online", "to": true},
                {"id": "c", "type": "if", "rules": [{"kind": "presence", "is": "someone"}]},
                {"id": "on", "type": "set", "device": "house:lamp", "point": "on", "value": true}
            ],
            "edges": [{"from": "t", "to": "c"}, {"from": "c", "port": "yes", "to": "on"}]
        }
    }));
    // The last person leaves: everything off.
    let away = automation(serde_json::json!({
        "name": "Départ", "enabled": true,
        "graph": {
            "nodes": [
                {"id": "t", "type": "presence", "event": "last_departure"},
                {"id": "o", "type": "lights_off"}
            ],
            "edges": [{"from": "t", "to": "o"}]
        }
    }));
    // By hand: lamp on, a long wait, then off; « restart » mode.
    let manual = automation(serde_json::json!({
        "name": "Manuel", "enabled": false,
        "graph": {
            "nodes": [
                {"id": "t", "type": "manual"},
                {"id": "a", "type": "set", "device": "house:lamp", "point": "on", "value": true},
                {"id": "w", "type": "wait", "seconds": 3600},
                {"id": "b", "type": "set", "device": "house:lamp", "point": "on", "value": false}
            ],
            "edges": [{"from": "t", "to": "a"}, {"from": "a", "to": "w"}, {"from": "w", "to": "b"}]
        }
    }));

    // A dry run reads the condition but commands nothing.
    let trial = automations.test(desk.clone()).await.unwrap();
    assert_eq!(trial.status, RunStatus::Done);
    let statuses: Vec<StepStatus> = trial.steps.iter().map(|step| step.status).collect();
    assert_eq!(
        statuses,
        [StepStatus::Done, StepStatus::Yes, StepStatus::Simulated]
    );
    assert_eq!(hub.value(&id("house:lamp"), "on"), Some(Value::Bool(false)));

    let saved = automations
        .replace(Document {
            automations: vec![desk, away, manual],
            ..Document::default()
        })
        .unwrap();
    assert!(
        saved
            .automations
            .iter()
            .all(|automation| !automation.id.is_empty())
    );
    automations.spawn();
    tokio::time::sleep(Duration::from_millis(50)).await;

    set(&hub, "house:pc", "online", true).await;
    wait_for(&hub, "house:lamp", "on", true).await;

    set(&hub, "house:thomas", "home", false).await;
    wait_for(&hub, "house:lamp", "on", false).await;

    // Nobody home now: the desk rule takes the « no » branch.
    set(&hub, "house:pc", "online", false).await;
    set(&hub, "house:pc", "online", true).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(hub.value(&id("house:lamp"), "on"), Some(Value::Bool(false)));
    let desk_runs = automations.runs(Some(&saved.automations[0].id), 10);
    assert_eq!(desk_runs[0].steps.last().unwrap().status, StepStatus::No);

    // A switched-off automation still runs by hand; a second start cancels the waiting one.
    let manual_id = saved.automations[2].id.clone();
    automations.run_now(&manual_id).unwrap();
    wait_for(&hub, "house:lamp", "on", true).await;
    automations.run_now(&manual_id).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let runs = automations.runs(Some(&manual_id), 10);
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs[1].status,
        RunStatus::Cancelled,
        "the first run was cancelled"
    );
    assert_eq!(runs[0].status, RunStatus::Running, "the second one waits");

    // « Régler un appareil »: the lamp as it should be, switched on before its brightness.
    let card = automation(serde_json::json!({
        "name": "Lecture", "enabled": false,
        "graph": {
            "nodes": [
                {"id": "t", "type": "manual"},
                {"id": "d", "type": "device", "device": "house:lamp", "values": {"brightness": 30, "on": true}},
                {"id": "f", "type": "device", "device": "house:pc", "toggle": "online"}
            ],
            "edges": [{"from": "t", "to": "d"}, {"from": "d", "to": "f"}]
        }
    }));
    let trial = automations.test(card.clone()).await.unwrap();
    assert_eq!(trial.steps[1].detail, "Lampe : allumé, Luminosité 30");
    assert_eq!(
        trial.steps[2].detail, "PC : hors ligne",
        "the computer is online: inverted"
    );
    let mut document = automations.document();
    document.automations.push(card);
    let saved = automations.replace(document).unwrap();
    set(&hub, "house:lamp", "on", false).await;
    automations.run_now(&saved.automations[3].id).unwrap();
    wait_for(&hub, "house:pc", "online", false).await;
    assert_eq!(hub.value(&id("house:lamp"), "on"), Some(Value::Bool(true)));
    assert_eq!(
        hub.value(&id("house:lamp"), "brightness"),
        Some(Value::Number(30.0))
    );

    assert!(dir.join("automations.json").exists());
    assert!(
        dir.join("automation-runs.jsonl").exists(),
        "finished runs are kept"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
