use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hestia_core::{Command, DeviceId, DeviceInfo, Origin, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::warn;

use crate::hub::Hub;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A protocol implementation. `run` is expected to loop forever: if it returns, fails or
/// panics, the supervisor marks its devices unavailable and restarts it with backoff.
pub trait Driver: Send + Sync + 'static {
    fn kind(&self) -> &'static str;

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>>;
}

/// A validated command waiting for its driver. Dropping it without an answer reports a failure.
pub struct PendingCommand {
    command: Command,
    origin: Origin,
    deadline: Instant,
    reply: oneshot::Sender<Result<(), String>>,
}

impl PendingCommand {
    pub(crate) fn new(
        command: Command,
        origin: Origin,
        deadline: Instant,
        reply: oneshot::Sender<Result<(), String>>,
    ) -> Self {
        Self {
            command,
            origin,
            deadline,
            reply,
        }
    }

    #[must_use]
    pub fn command(&self) -> &Command {
        &self.command
    }

    #[must_use]
    pub fn origin(&self) -> Origin {
        self.origin
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub fn succeed(self) {
        // The caller may have given up already; nothing left to tell.
        let _ = self.reply.send(Ok(()));
    }

    pub fn fail(self, reason: impl Into<String>) {
        let _ = self.reply.send(Err(reason.into()));
    }
}

/// A driver's only door to the hub, scoped to its own instance.
pub struct DriverContext {
    instance: String,
    hub: Arc<Hub>,
    commands: mpsc::Receiver<PendingCommand>,
}

impl DriverContext {
    pub(crate) fn new(hub: Arc<Hub>, instance: String) -> Self {
        let commands = hub.register_driver(&instance);
        Self {
            instance,
            hub,
            commands,
        }
    }

    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// # Errors
    /// Fails when `native` is empty or contains whitespace.
    pub fn device_id(&self, native: &str) -> anyhow::Result<DeviceId> {
        Ok(DeviceId::new(&self.instance, native)?)
    }

    pub fn upsert_device(&self, info: DeviceInfo) {
        if self.owns(&info.id) {
            self.hub.upsert_device(info);
        }
    }

    pub fn set_state(&self, device: &DeviceId, point: &str, value: Value) {
        if self.owns(device) {
            self.hub.set_state(device, point, value);
        }
    }

    pub fn set_available(&self, device: &DeviceId, available: bool) {
        if self.owns(device) {
            self.hub.set_available(device, available);
        }
    }

    /// Forgets a device the user removed, as opposed to one merely unreachable.
    pub fn remove_device(&self, device: &DeviceId) {
        if self.owns(device) {
            self.hub.remove_device(device);
        }
    }

    /// Next command to execute. Commands whose deadline passed are refused, never run late.
    pub async fn next_command(&mut self) -> PendingCommand {
        loop {
            match self.commands.recv().await {
                Some(pending) if pending.is_expired() => {
                    warn!(instance = self.instance, device = %pending.command.device, "stale command dropped");
                    pending.fail("expired before execution");
                }
                Some(pending) => return pending,
                None => std::future::pending::<()>().await,
            }
        }
    }

    pub(crate) fn refuse_queued(&mut self, reason: &str) {
        while let Ok(pending) = self.commands.try_recv() {
            pending.fail(reason);
        }
    }

    fn owns(&self, device: &DeviceId) -> bool {
        let owned = device.instance() == self.instance;
        if !owned {
            warn!(instance = self.instance, device = %device, "driver touched a device it does not own");
        }
        owned
    }
}
