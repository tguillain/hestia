//! `HomeKit` Accessory Protocol bridge (HAP over IP). Exposes the hub's lights to Apple Home,
//! entirely on the local network.

mod accessories;
mod bridge;
mod color;
mod connection;
mod crypto;
mod http;
mod mdns;
mod pair_setup;
mod pair_verify;
mod pairings;
mod session;
mod srp;
mod state;
mod tlv;

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use hestia_core::DeviceId;
use hestia_hub::{Event, Hub};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

use crate::bridge::Bridge;
use crate::mdns::Advertiser;
use crate::state::BridgeState;

const MAX_CONNECTIONS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HomeKitConfig {
    pub enabled: bool,
    /// Bridge name shown while pairing.
    pub name: String,
    pub port: u16,
    /// Lights never exposed to Apple Home.
    pub exclude: Vec<DeviceId>,
}

impl Default for HomeKitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            name: "Hestia".to_owned(),
            port: 51827,
            exclude: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeKitStatus {
    pub setup_code: String,
    pub setup_uri: String,
    pub paired: bool,
    pub controllers: usize,
    pub accessories: usize,
}

pub struct HomeKit {
    bridge: Arc<Bridge>,
}

impl HomeKit {
    /// Opens the bridge identity, starts listening and announces the bridge on the LAN.
    ///
    /// # Errors
    /// Fails when the state file cannot be used, the port is taken or mDNS cannot start.
    pub async fn start(
        hub: Arc<Hub>,
        config: &HomeKitConfig,
        data_dir: &Path,
    ) -> anyhow::Result<Self> {
        let state = BridgeState::open(data_dir)?;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.port))
            .await
            .with_context(|| format!("cannot listen for HomeKit on port {}", config.port))?;
        let advertiser = Advertiser::new(&config.name, state.device_id(), config.port)?;
        let excluded: HashSet<DeviceId> = config.exclude.iter().cloned().collect();
        let bridge = Arc::new(Bridge::new(
            hub,
            config.name.clone(),
            state,
            advertiser,
            excluded,
        ));
        bridge.sync_lights();
        bridge.advertise();
        tokio::spawn(accept(Arc::clone(&bridge), listener));
        tokio::spawn(follow_hub(Arc::clone(&bridge)));

        let homekit = Self { bridge };
        let status = homekit.status();
        if status.paired {
            info!(
                port = config.port,
                controllers = status.controllers,
                "HomeKit bridge ready"
            );
        } else {
            info!(
                port = config.port,
                code = status.setup_code,
                "HomeKit bridge waiting for pairing"
            );
        }
        Ok(homekit)
    }

    /// Bridge name shown while pairing.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.bridge.name
    }

    #[must_use]
    pub fn status(&self) -> HomeKitStatus {
        status_of(&self.bridge.state())
    }
}

/// Reads the pairing status without starting the bridge (CLI).
///
/// # Errors
/// Fails when the state file cannot be read or created.
pub fn offline_status(data_dir: &Path) -> anyhow::Result<HomeKitStatus> {
    Ok(status_of(&BridgeState::open(data_dir)?))
}

/// Forgets every pairing and the bridge identity; Apple Home will see a brand new bridge.
/// The service must be stopped first, or it would write its state back.
///
/// # Errors
/// Fails when the state file cannot be removed.
pub fn reset(data_dir: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(data_dir.join("homekit.json")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Removes a light from Apple Home. The service must be stopped first.
///
/// # Errors
/// Fails when the state file cannot be read or written.
pub fn forget(data_dir: &Path, device: &DeviceId) -> anyhow::Result<bool> {
    BridgeState::open(data_dir)?.forget_light(device)
}

fn status_of(state: &BridgeState) -> HomeKitStatus {
    HomeKitStatus {
        setup_code: state.setup_code().to_owned(),
        setup_uri: state.setup_uri(),
        paired: state.is_paired(),
        controllers: state.pairings().len(),
        accessories: state.lights().len(),
    }
}

async fn accept(bridge: Arc<Bridge>, listener: TcpListener) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let next_id = AtomicU64::new(1);
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                warn!(%error, "HomeKit accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
        };
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            warn!("too many HomeKit connections, refusing one");
            continue;
        };
        let _ = stream.set_nodelay(true);
        let bridge = Arc::clone(&bridge);
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            connection::serve(bridge, stream, id).await;
            drop(slot);
        });
    }
}

async fn follow_hub(bridge: Arc<Bridge>) {
    let mut events = bridge.hub.subscribe();
    loop {
        match events.recv().await {
            Ok(Event::DeviceUpdated { .. }) => bridge.sync_lights(),
            Ok(Event::DeviceRemoved { device }) => bridge.forget_light(&device),
            Ok(Event::State { device, point, .. }) => bridge.notify(&device, &point),
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return,
        }
    }
}
