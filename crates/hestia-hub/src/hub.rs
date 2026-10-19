use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use hestia_core::{Command, DeviceId, DeviceInfo, Origin, PointSpec, Value, ValueError};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::Instant;
use tracing::{info, warn};

use crate::driver::PendingCommand;
use crate::event::{CommandOutcome, Event, now_millis};

/// A command not executed within this delay is abandoned, never run late.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_CAPACITY: usize = 256;
const COMMAND_QUEUE: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Label {
    pub name: Option<String>,
    pub room: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceSnapshot {
    pub id: DeviceId,
    pub name: String,
    pub room: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub available: bool,
    pub points: Vec<PointSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PointSnapshot {
    #[serde(flatten)]
    pub spec: PointSpec,
    pub value: Option<Value>,
    pub at: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("unknown device {0}")]
    UnknownDevice(DeviceId),
    #[error("device {device} has no point {point:?}")]
    UnknownPoint { device: DeviceId, point: String },
    #[error("point {0:?} is read-only")]
    ReadOnly(String),
    #[error(transparent)]
    Invalid(#[from] ValueError),
    #[error("device {0} is unavailable")]
    Unavailable(DeviceId),
    #[error("driver queue is full")]
    Busy,
    #[error("driver did not answer in time")]
    Timeout,
    #[error("driver stopped")]
    DriverStopped,
    #[error("command failed: {0}")]
    Failed(String),
}

struct Sample {
    value: Value,
    at: i64,
}

struct DeviceEntry {
    info: DeviceInfo,
    available: bool,
    values: BTreeMap<String, Sample>,
}

#[derive(Default)]
struct Inner {
    devices: BTreeMap<DeviceId, DeviceEntry>,
    drivers: HashMap<String, mpsc::Sender<PendingCommand>>,
}

pub struct Hub {
    inner: RwLock<Inner>,
    labels: HashMap<DeviceId, Label>,
    events: broadcast::Sender<Event>,
}

impl Hub {
    #[must_use]
    pub fn new(labels: HashMap<DeviceId, Label>) -> Arc<Self> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Arc::new(Self {
            inner: RwLock::new(Inner::default()),
            labels,
            events,
        })
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    #[must_use]
    pub fn devices(&self) -> Vec<DeviceSnapshot> {
        self.read()
            .devices
            .values()
            .map(|entry| self.snapshot(entry))
            .collect()
    }

    #[must_use]
    pub fn device(&self, id: &DeviceId) -> Option<DeviceSnapshot> {
        self.read()
            .devices
            .get(id)
            .map(|entry| self.snapshot(entry))
    }

    #[must_use]
    pub fn value(&self, id: &DeviceId, point: &str) -> Option<Value> {
        self.read()
            .devices
            .get(id)
            .and_then(|entry| entry.values.get(point))
            .map(|sample| sample.value.clone())
    }

    /// Validates `command`, hands it to the owning driver and waits for its answer.
    ///
    /// # Errors
    /// Returns why the command was refused or did not complete.
    pub async fn command(&self, command: Command, origin: Origin) -> Result<(), CommandError> {
        let result = self.dispatch(&command, origin).await;
        match &result {
            Ok(()) => {
                info!(device = %command.device, point = command.point, ?origin, "command done");
            }
            Err(error) => {
                warn!(device = %command.device, point = command.point, ?origin, %error, "command refused");
            }
        }
        self.emit(Event::Command {
            outcome: CommandOutcome::of(&result),
            device: command.device,
            point: command.point,
            value: command.value,
            origin,
        });
        result
    }

    async fn dispatch(&self, command: &Command, origin: Origin) -> Result<(), CommandError> {
        let sender = self.route(command)?;
        let (reply, answer) = oneshot::channel();
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let pending = PendingCommand::new(command.clone(), origin, deadline, reply);
        match sender.try_send(pending) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => return Err(CommandError::Busy),
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(CommandError::DriverStopped),
        }
        match tokio::time::timeout_at(deadline, answer).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(reason))) => Err(CommandError::Failed(reason)),
            Ok(Err(_)) => Err(CommandError::DriverStopped),
            Err(_) => Err(CommandError::Timeout),
        }
    }

    fn route(&self, command: &Command) -> Result<mpsc::Sender<PendingCommand>, CommandError> {
        let inner = self.read();
        let entry = inner
            .devices
            .get(&command.device)
            .ok_or_else(|| CommandError::UnknownDevice(command.device.clone()))?;
        let spec = entry
            .info
            .point(&command.point)
            .ok_or_else(|| CommandError::UnknownPoint {
                device: command.device.clone(),
                point: command.point.clone(),
            })?;
        if !spec.is_writable() {
            return Err(CommandError::ReadOnly(command.point.clone()));
        }
        spec.validate(&command.value)?;
        if !entry.available {
            return Err(CommandError::Unavailable(command.device.clone()));
        }
        inner
            .drivers
            .get(command.device.instance())
            .cloned()
            .ok_or(CommandError::DriverStopped)
    }

    pub(crate) fn register_driver(&self, instance: &str) -> mpsc::Receiver<PendingCommand> {
        let (sender, receiver) = mpsc::channel(COMMAND_QUEUE);
        self.write().drivers.insert(instance.to_owned(), sender);
        receiver
    }

    pub(crate) fn upsert_device(&self, info: DeviceInfo) {
        let id = info.id.clone();
        {
            let mut inner = self.write();
            match inner.devices.get_mut(&id) {
                Some(entry) => {
                    entry.values.retain(|key, _| info.point(key).is_some());
                    entry.info = info;
                }
                None => {
                    inner.devices.insert(
                        id.clone(),
                        DeviceEntry {
                            info,
                            available: false,
                            values: BTreeMap::new(),
                        },
                    );
                }
            }
        }
        self.emit(Event::DeviceUpdated { device: id });
    }

    pub(crate) fn set_state(&self, id: &DeviceId, point: &str, value: Value) {
        let at = now_millis();
        {
            let mut inner = self.write();
            let Some(entry) = inner.devices.get_mut(id) else {
                warn!(device = %id, point, "state for an undeclared device ignored");
                return;
            };
            let Some(spec) = entry.info.point(point) else {
                warn!(device = %id, point, "state for an undeclared point ignored");
                return;
            };
            if let Err(error) = spec.validate(&value) {
                warn!(device = %id, point, %error, "invalid state ignored");
                return;
            }
            if entry
                .values
                .get(point)
                .is_some_and(|sample| sample.value == value)
            {
                return;
            }
            entry.values.insert(
                point.to_owned(),
                Sample {
                    value: value.clone(),
                    at,
                },
            );
        }
        self.emit(Event::State {
            device: id.clone(),
            point: point.to_owned(),
            value,
            at,
        });
    }

    pub(crate) fn set_available(&self, id: &DeviceId, available: bool) {
        {
            let mut inner = self.write();
            let Some(entry) = inner.devices.get_mut(id) else {
                return;
            };
            if entry.available == available {
                return;
            }
            entry.available = available;
        }
        self.emit(Event::Availability {
            device: id.clone(),
            available,
        });
    }

    pub(crate) fn remove_device(&self, id: &DeviceId) {
        if self.write().devices.remove(id).is_none() {
            return;
        }
        self.emit(Event::DeviceRemoved { device: id.clone() });
    }

    pub(crate) fn mark_driver_down(&self, instance: &str) {
        let owned: Vec<DeviceId> = self
            .read()
            .devices
            .keys()
            .filter(|id| id.instance() == instance)
            .cloned()
            .collect();
        for id in owned {
            self.set_available(&id, false);
        }
    }

    fn snapshot(&self, entry: &DeviceEntry) -> DeviceSnapshot {
        let label = self.labels.get(&entry.info.id);
        DeviceSnapshot {
            id: entry.info.id.clone(),
            name: label
                .and_then(|label| label.name.clone())
                .unwrap_or_else(|| entry.info.name.clone()),
            room: label.and_then(|label| label.room.clone()),
            manufacturer: entry.info.manufacturer.clone(),
            model: entry.info.model.clone(),
            available: entry.available,
            points: entry
                .info
                .points
                .iter()
                .map(|spec| {
                    let sample = entry.values.get(&spec.key);
                    PointSnapshot {
                        spec: spec.clone(),
                        value: sample.map(|sample| sample.value.clone()),
                        at: sample.map(|sample| sample.at),
                    }
                })
                .collect(),
        }
    }

    fn emit(&self, event: Event) {
        // No subscriber is a normal situation (nothing connected yet).
        let _ = self.events.send(event);
    }

    fn read(&self) -> RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }
}
