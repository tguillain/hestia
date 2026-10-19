use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use hestia_core::DeviceId;
use hestia_hub::{BoxFuture, Driver, DriverContext, PendingCommand};
use serde::Serialize;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{self, Instant, MissedTickBehavior};
use tracing::{info, warn};

use crate::config::MatterConfig;
use crate::light::{self, device_info, state_attributes, state_values};
use crate::stack::{SetupCode, Stack, Target};
use crate::store::{Controller, PairedDevice, load_controller, native_id, save_controller};

/// A device missing this many reads in a row is reported unavailable.
const MISSED_READS: u32 = 3;
/// An unreachable device is read less and less often, up to this many poll periods apart.
const MAX_SKIPPED_POLLS: u32 = 12;

pub struct MatterDriver {
    config: MatterConfig,
    data_dir: PathBuf,
    requests: Option<Arc<Mutex<mpsc::Receiver<Request>>>>,
}

/// The Settings page's door to the driver: pairing and removing devices.
#[derive(Clone)]
pub struct Pairing {
    requests: mpsc::Sender<Request>,
}

/// The driver's end of [`Pairing`].
pub struct PairingRequests(Arc<Mutex<mpsc::Receiver<Request>>>);

#[must_use]
pub fn pairing_channel() -> (Pairing, PairingRequests) {
    let (requests, receiver) = mpsc::channel(4);
    (
        Pairing { requests },
        PairingRequests(Arc::new(Mutex::new(receiver))),
    )
}

/// A device as the Settings page lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairedSummary {
    /// Hub id, e.g. `matter:1a2b3c4d5e6f7081`.
    pub id: String,
    pub vendor: String,
    pub product: String,
    pub address: Option<String>,
}

pub enum Request {
    Pair {
        code: String,
        address: Option<SocketAddr>,
        reply: oneshot::Sender<Result<PairedSummary, String>>,
    },
    Remove {
        native_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

impl Pairing {
    /// Commissions the device behind `code` onto Hestia's fabric. Takes up to two minutes.
    ///
    /// # Errors
    /// Fails with a message for the household.
    pub async fn pair(
        &self,
        code: String,
        address: Option<SocketAddr>,
    ) -> Result<PairedSummary, String> {
        let (reply, answer) = oneshot::channel();
        self.send(Request::Pair {
            code,
            address,
            reply,
        })
        .await?;
        answer.await.map_err(|_| STOPPED.to_owned())?
    }

    /// Removes Hestia from the device and forgets it.
    ///
    /// # Errors
    /// Fails when the device is unknown or the driver is not running.
    pub async fn remove(&self, native_id: String) -> Result<(), String> {
        let (reply, answer) = oneshot::channel();
        self.send(Request::Remove { native_id, reply }).await?;
        answer.await.map_err(|_| STOPPED.to_owned())?
    }

    async fn send(&self, request: Request) -> Result<(), String> {
        self.requests
            .send(request)
            .await
            .map_err(|_| STOPPED.to_owned())
    }
}

const STOPPED: &str = "le pilote Matter ne fonctionne pas";

/// Paired devices as stored, for the Settings page.
///
/// # Errors
/// Fails when the file cannot be read.
pub fn paired_devices(
    data_dir: &std::path::Path,
    instance: &str,
) -> anyhow::Result<Vec<PairedSummary>> {
    Ok(load_controller(data_dir)?
        .devices
        .iter()
        .map(|device| summary(instance, device))
        .collect())
}

fn summary(instance: &str, device: &PairedDevice) -> PairedSummary {
    PairedSummary {
        id: format!("{instance}:{}", device.native_id()),
        vendor: device.vendor.clone(),
        product: device.product.clone(),
        address: device.address.map(|address| match address.ip() {
            IpAddr::V6(v6) => v6
                .to_ipv4_mapped()
                .map_or_else(|| v6.to_string(), |v4| v4.to_string()),
            IpAddr::V4(v4) => v4.to_string(),
        }),
    }
}

/// Read failures in a row, and when to try again.
#[derive(Default)]
struct Health {
    failures: u32,
    next_read: Option<Instant>,
}

impl MatterDriver {
    #[must_use]
    pub fn new(config: MatterConfig, data_dir: PathBuf) -> Self {
        Self {
            config,
            data_dir,
            requests: None,
        }
    }

    #[must_use]
    pub fn with_pairing(mut self, requests: &PairingRequests) -> Self {
        self.requests = Some(Arc::clone(&requests.0));
        self
    }

    async fn serve(&self, ctx: &mut DriverContext) -> anyhow::Result<()> {
        let mut controller = load_controller(&self.data_dir)?;
        let (stack, keys) = Stack::start(controller.fabric.clone()).await?;
        if controller.fabric.as_ref() != Some(&keys) {
            controller.fabric = Some(keys);
            save_controller(&self.data_dir, &controller)?;
        }
        for device in &controller.devices {
            ctx.upsert_device(device_info(ctx.device_id(&device.native_id())?, device));
        }
        info!(
            devices = controller.devices.len(),
            "Matter controller ready"
        );
        // Held for the driver's lifetime: a restarted driver takes it over.
        let mut requests = match &self.requests {
            Some(requests) => Some(requests.lock().await),
            None => None,
        };
        let mut health: HashMap<u64, Health> = HashMap::new();
        let mut poll = time::interval(self.config.poll_interval());
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    let nodes: Vec<u64> = controller.devices.iter().map(|device| device.node_id).collect();
                    for node_id in nodes {
                        let entry = health.entry(node_id).or_default();
                        if entry.next_read.is_none_or(|next| Instant::now() >= next) {
                            self.refresh(ctx, &stack, &mut controller, entry, node_id).await;
                        }
                    }
                }
                pending = ctx.next_command() => {
                    self.execute(ctx, &stack, &mut controller, &mut health, pending).await;
                }
                Some(request) = next_request(&mut requests) => {
                    self.handle(ctx, &stack, &mut controller, &mut health, request).await;
                }
            }
        }
    }

    /// Reads one device and publishes its state.
    async fn refresh(
        &self,
        ctx: &DriverContext,
        stack: &Stack,
        controller: &mut Controller,
        health: &mut Health,
        node_id: u64,
    ) {
        let Some(device) = controller
            .devices
            .iter()
            .find(|device| device.node_id == node_id)
        else {
            return;
        };
        let Ok(id) = ctx.device_id(&device.native_id()) else {
            return;
        };
        let features = device.features;
        let attributes = state_attributes(&features);
        match stack.read(target(device, health), attributes).await {
            Ok(answer) => {
                for (point, value) in state_values(&answer.value, &features) {
                    ctx.set_state(&id, point, value);
                }
                ctx.set_available(&id, true);
                *health = Health::default();
                self.remember_address(controller, node_id, answer.address);
            }
            Err(error) => {
                health.failures += 1;
                let skipped = health.failures.min(MAX_SKIPPED_POLLS);
                health.next_read = Some(Instant::now() + self.config.poll_interval() * skipped);
                if health.failures == MISSED_READS {
                    warn!(device = %id, %error, "Matter device unreachable");
                }
                if health.failures >= MISSED_READS {
                    ctx.set_available(&id, false);
                }
            }
        }
    }

    async fn execute(
        &self,
        ctx: &DriverContext,
        stack: &Stack,
        controller: &mut Controller,
        health: &mut HashMap<u64, Health>,
        pending: PendingCommand,
    ) {
        let command = pending.command();
        let Some(device) = find(ctx, controller, &command.device) else {
            pending.fail("unknown Matter device");
            return;
        };
        let action = match light::action(&command.point, &command.value, &device.features) {
            Ok(action) => action,
            Err(reason) => {
                pending.fail(reason);
                return;
            }
        };
        let node_id = device.node_id;
        let entry = health.entry(node_id).or_default();
        match stack.invoke(target(device, entry), action).await {
            Ok(answer) => {
                pending.succeed();
                self.remember_address(controller, node_id, answer.address);
                // Show the result right away rather than at the next poll.
                *entry = Health::default();
                self.refresh(ctx, stack, controller, entry, node_id).await;
            }
            Err(error) => {
                entry.failures += 1;
                pending.fail(error);
            }
        }
    }

    async fn handle(
        &self,
        ctx: &DriverContext,
        stack: &Stack,
        controller: &mut Controller,
        health: &mut HashMap<u64, Health>,
        request: Request,
    ) {
        match request {
            Request::Pair {
                code,
                address,
                reply,
            } => {
                let result = self.pair(ctx, stack, controller, &code, address).await;
                if let Ok(paired) = &result {
                    info!(device = paired.id, "Matter device paired");
                }
                let _ = reply.send(result);
            }
            Request::Remove { native_id, reply } => {
                let Some(position) = controller
                    .devices
                    .iter()
                    .position(|device| device.native_id() == native_id)
                else {
                    let _ = reply.send(Err(format!("appareil matter:{native_id} inconnu")));
                    return;
                };
                let device = controller.devices.remove(position);
                if let Err(error) = save_controller(&self.data_dir, controller) {
                    controller.devices.insert(position, device);
                    let _ = reply.send(Err(format!("impossible d'enregistrer : {error:#}")));
                    return;
                }
                let entry = health.remove(&device.node_id).unwrap_or_default();
                // Best effort: an unreachable device keeps a stale slot, nothing worse.
                if let Err(error) = stack.unpair(target(&device, &entry)).await {
                    warn!(node = native_id, %error, "Matter device not told it was removed");
                }
                if let Ok(id) = ctx.device_id(&native_id) {
                    ctx.set_available(&id, false);
                }
                info!(node = native_id, "Matter device removed");
                let _ = reply.send(Ok(()));
            }
        }
    }

    async fn pair(
        &self,
        ctx: &DriverContext,
        stack: &Stack,
        controller: &mut Controller,
        code: &str,
        address: Option<SocketAddr>,
    ) -> Result<PairedSummary, String> {
        let code = SetupCode::parse(code)?;
        let node_id = loop {
            // Operational node ids stop at 0xFFFF_FFEF_FFFF_FFFF.
            let candidate = (rand::random::<u64>() >> 4).max(1);
            let taken = controller.fabric.as_ref().map(|fabric| fabric.node_id) == Some(candidate)
                || controller
                    .devices
                    .iter()
                    .any(|device| device.node_id == candidate);
            if !taken {
                break candidate;
            }
        };
        let device = stack
            .pair(code, address, node_id)
            .await
            .map_err(|error| pairing_message(&error, address.is_some()))?;
        let id = ctx
            .device_id(&native_id(device.node_id))
            .map_err(|error| error.to_string())?;
        controller.devices.push(device.clone());
        save_controller(&self.data_dir, controller)
            .map_err(|error| format!("appairé, mais impossible d'enregistrer : {error:#}"))?;
        ctx.upsert_device(device_info(id, &device));
        let mut health = Health::default();
        self.refresh(ctx, stack, controller, &mut health, device.node_id)
            .await;
        Ok(summary(ctx.instance(), &device))
    }

    fn remember_address(
        &self,
        controller: &mut Controller,
        node_id: u64,
        address: Option<std::net::SocketAddr>,
    ) {
        let Some(address) = address else {
            return;
        };
        let Some(device) = controller
            .devices
            .iter_mut()
            .find(|device| device.node_id == node_id)
        else {
            return;
        };
        if device.address == Some(address) {
            return;
        }
        device.address = Some(address);
        if let Err(error) = save_controller(&self.data_dir, controller) {
            warn!(%error, "cannot remember Matter device address");
        }
    }
}

impl Driver for MatterDriver {
    fn kind(&self) -> &'static str {
        "matter"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.serve(ctx))
    }
}

async fn next_request(
    requests: &mut Option<tokio::sync::MutexGuard<'_, mpsc::Receiver<Request>>>,
) -> Option<Request> {
    match requests {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn find<'c>(
    ctx: &DriverContext,
    controller: &'c Controller,
    id: &DeviceId,
) -> Option<&'c PairedDevice> {
    controller.devices.iter().find(|device| {
        ctx.device_id(&device.native_id())
            .is_ok_and(|candidate| &candidate == id)
    })
}

/// After a failure the remembered address may be stale: every other attempt asks mDNS.
fn target(device: &PairedDevice, health: &Health) -> Target {
    Target {
        node_id: device.node_id,
        endpoint: device.endpoint,
        address: device.address.filter(|_| health.failures.is_multiple_of(2)),
    }
}

fn pairing_message(error: &str, by_address: bool) -> String {
    if error.contains("NotFound") && !by_address {
        return "Aucun appareil Matter en attente d'appairage n'a été trouvé. Ouvre l'appairage \
                dans l'app du fabricant puis réessaie, ou indique l'adresse IP de l'appareil."
            .to_owned();
    }
    format!("L'appairage a échoué : {error}")
}

#[cfg(test)]
mod tests {
    use super::{Health, pairing_message, target};
    use crate::store::{Features, PairedDevice};

    #[test]
    fn remembered_address_alternates_with_mdns() {
        let device = PairedDevice {
            node_id: 7,
            vendor: String::new(),
            product: String::new(),
            endpoint: 1,
            features: Features::default(),
            address: Some("[::ffff:192.168.1.40]:5540".parse().unwrap()),
        };
        let mut health = Health::default();
        assert!(target(&device, &health).address.is_some());
        health.failures = 1;
        assert!(target(&device, &health).address.is_none());
        health.failures = 2;
        assert!(target(&device, &health).address.is_some());
    }

    #[test]
    fn not_found_suggests_the_app() {
        assert!(pairing_message("NotFound", false).contains("app du fabricant"));
        assert!(pairing_message("NotFound", true).starts_with("L'appairage a échoué"));
    }
}
