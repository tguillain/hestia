use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hestia_core::{Command, DeviceId, DeviceInfo, Origin, PointKind, PointSpec, Semantic, Value};
use hestia_hub::{
    BoxFuture, CommandError, CommandOutcome, Driver, DriverContext, Event, Hub, Label, spawn_driver,
};

struct FakeLamp {
    panic_on_next_command: AtomicBool,
    remove_on_next_command: AtomicBool,
}

impl FakeLamp {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            panic_on_next_command: AtomicBool::new(false),
            remove_on_next_command: AtomicBool::new(false),
        })
    }
}

fn lamp_info(id: DeviceId) -> DeviceInfo {
    DeviceInfo {
        id,
        name: "Lampe".to_owned(),
        manufacturer: None,
        model: None,
        points: vec![
            PointSpec::new("on", "Allumage", PointKind::Binary, Semantic::OnOff).writable(),
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
            .writable(),
            PointSpec::new(
                "mode",
                "Mode",
                PointKind::Enum {
                    values: vec!["color".to_owned(), "white".to_owned()],
                },
                Semantic::ColorMode,
            ),
        ],
    }
}

impl Driver for FakeLamp {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("lamp")?;
            ctx.upsert_device(lamp_info(id.clone()));
            ctx.set_state(&id, "on", Value::Bool(false));
            ctx.set_available(&id, true);
            loop {
                let pending = ctx.next_command().await;
                assert!(
                    !self.panic_on_next_command.swap(false, Ordering::SeqCst),
                    "simulated driver bug"
                );
                let command = pending.command().clone();
                if self.remove_on_next_command.swap(false, Ordering::SeqCst) {
                    ctx.remove_device(&"other:lamp".parse().unwrap());
                    ctx.remove_device(&command.device);
                } else {
                    ctx.set_state(&command.device, &command.point, command.value);
                }
                pending.succeed();
            }
        })
    }
}

fn lamp_id() -> DeviceId {
    "fake:lamp".parse().unwrap()
}

fn set(point: &str, value: Value) -> Command {
    Command {
        device: lamp_id(),
        point: point.to_owned(),
        value,
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..1_000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never became true");
}

async fn started_hub(driver: Arc<FakeLamp>) -> Arc<Hub> {
    let labels = HashMap::from([(
        lamp_id(),
        Label {
            name: Some("Chambre".to_owned()),
            room: Some("Chambre".to_owned()),
        },
    )]);
    let hub = Hub::new(labels);
    spawn_driver(&hub, "fake", driver);
    let watched = Arc::clone(&hub);
    wait_until(move || {
        watched
            .device(&lamp_id())
            .is_some_and(|device| device.available)
    })
    .await;
    hub
}

#[tokio::test(start_paused = true)]
async fn command_reaches_driver_and_updates_state() {
    let hub = started_hub(FakeLamp::new()).await;
    let mut events = hub.subscribe();

    hub.command(set("on", Value::Bool(true)), Origin::Ui)
        .await
        .unwrap();

    assert_eq!(hub.value(&lamp_id(), "on"), Some(Value::Bool(true)));
    let snapshot = hub.device(&lamp_id()).unwrap();
    assert_eq!(snapshot.name, "Chambre");
    assert_eq!(snapshot.room.as_deref(), Some("Chambre"));

    let mut saw_state = false;
    let mut saw_command = false;
    while let Ok(event) = events.try_recv() {
        match event {
            Event::State { point, value, .. } if point == "on" => {
                saw_state = value == Value::Bool(true);
            }
            Event::Command {
                origin, outcome, ..
            } => saw_command = origin == Origin::Ui && outcome == CommandOutcome::Done,
            _ => {}
        }
    }
    assert!(saw_state && saw_command);
}

#[tokio::test(start_paused = true)]
async fn invalid_commands_never_reach_the_driver() {
    let hub = started_hub(FakeLamp::new()).await;

    let unknown_device = Command {
        device: "fake:ghost".parse().unwrap(),
        ..set("on", Value::Bool(true))
    };
    assert!(matches!(
        hub.command(unknown_device, Origin::Api).await,
        Err(CommandError::UnknownDevice(_))
    ));
    assert!(matches!(
        hub.command(set("color", Value::Bool(true)), Origin::Api)
            .await,
        Err(CommandError::UnknownPoint { .. })
    ));
    assert!(matches!(
        hub.command(set("mode", Value::Text("white".to_owned())), Origin::Api)
            .await,
        Err(CommandError::ReadOnly(_))
    ));
    assert!(matches!(
        hub.command(set("brightness", Value::Number(150.0)), Origin::Api)
            .await,
        Err(CommandError::Invalid(_))
    ));
    assert!(matches!(
        hub.command(set("on", Value::Number(1.0)), Origin::Api)
            .await,
        Err(CommandError::Invalid(_))
    ));
    assert_eq!(hub.value(&lamp_id(), "brightness"), None);
}

#[tokio::test(start_paused = true)]
async fn panicking_driver_is_restarted() {
    let driver = FakeLamp::new();
    let hub = started_hub(Arc::clone(&driver)).await;

    driver.panic_on_next_command.store(true, Ordering::SeqCst);
    assert!(matches!(
        hub.command(set("on", Value::Bool(true)), Origin::Ui).await,
        Err(CommandError::DriverStopped)
    ));
    assert!(!hub.device(&lamp_id()).unwrap().available);

    let watched = Arc::clone(&hub);
    wait_until(move || {
        watched
            .device(&lamp_id())
            .is_some_and(|device| device.available)
    })
    .await;
    hub.command(set("brightness", Value::Number(40.0)), Origin::Ui)
        .await
        .unwrap();
    assert_eq!(
        hub.value(&lamp_id(), "brightness"),
        Some(Value::Number(40.0))
    );
}

#[tokio::test(start_paused = true)]
async fn removed_device_is_forgotten() {
    let driver = FakeLamp::new();
    let hub = started_hub(Arc::clone(&driver)).await;
    spawn_driver(&hub, "other", FakeLamp::new());
    let other: DeviceId = "other:lamp".parse().unwrap();
    let watched = Arc::clone(&hub);
    let watched_id = other.clone();
    wait_until(move || {
        watched
            .device(&watched_id)
            .is_some_and(|device| device.available)
    })
    .await;
    let mut events = hub.subscribe();

    driver.remove_on_next_command.store(true, Ordering::SeqCst);
    hub.command(set("on", Value::Bool(true)), Origin::Ui)
        .await
        .unwrap();

    assert!(hub.device(&lamp_id()).is_none());
    assert_eq!(hub.value(&lamp_id(), "on"), None);
    assert!(
        hub.device(&other).is_some(),
        "a driver cannot remove another driver's device"
    );
    let mut removed = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let Event::DeviceRemoved { device } = event {
            removed.push(device);
        }
    }
    assert_eq!(removed, [lamp_id()]);
    assert!(matches!(
        hub.command(set("on", Value::Bool(false)), Origin::Ui).await,
        Err(CommandError::UnknownDevice(_))
    ));
}
