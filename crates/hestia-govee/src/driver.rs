use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use hestia_core::{DeviceId, DeviceInfo, PointKind, PointSpec, Semantic, Unit, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext, PendingCommand};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::{self, Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::config::GoveeConfig;
use crate::discover::{bind, send, send_scan};
use crate::known::{KnownLamps, load_known, neighbours, save_known};
use crate::protocol::{self, Incoming, Outgoing, ScanReply, StatusReply};

const MIN_KELVIN: u32 = 2000;
const MAX_KELVIN: u32 = 9000;
/// A lamp missing this many polls in a row is reported unavailable.
const MISSED_POLLS: u32 = 3;
const FIRST_RESCAN: Duration = Duration::from_secs(2);
/// The whole-network sweep runs once shortly after start-up, then on request.
const FIRST_SWEEP: Duration = Duration::from_secs(5);
const NEVER: Duration = Duration::from_secs(365 * 24 * 3600);

pub struct GoveeDriver {
    config: GoveeConfig,
    data_dir: Option<PathBuf>,
    discovery: Option<watch::Receiver<u64>>,
}

struct Lamp {
    ip: Ipv4Addr,
    last_seen: Option<Instant>,
}

impl GoveeDriver {
    #[must_use]
    pub fn new(config: GoveeConfig) -> Self {
        Self {
            config,
            data_dir: None,
            discovery: None,
        }
    }

    /// Remembers lamp addresses in `data_dir` and reads the ones added by hand.
    #[must_use]
    pub fn with_memory(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    /// Sweeps the network each time the counter changes ("Rechercher des appareils").
    #[must_use]
    pub fn with_discovery(mut self, requests: watch::Receiver<u64>) -> Self {
        self.discovery = Some(requests);
        self
    }

    async fn serve(&self, ctx: &mut DriverContext) -> anyhow::Result<()> {
        let socket = bind(&self.config).await?;
        let mut lamps: BTreeMap<DeviceId, Lamp> = BTreeMap::new();
        let mut known = self.known();
        // UDP replies get lost: scan often at start-up, then settle to the configured pace.
        let mut scan_delay = FIRST_RESCAN;
        let scan = time::sleep(Duration::ZERO);
        tokio::pin!(scan);
        let sweep = time::sleep(FIRST_SWEEP);
        tokio::pin!(sweep);
        let mut discovery = self.discovery.clone();
        let mut poll = time::interval(self.config.poll_interval());
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut buffer = vec![0_u8; 2048];
        loop {
            tokio::select! {
                () = &mut scan => {
                    known = self.known();
                    send_scan(&socket, &self.config).await;
                    for address in known.addresses() {
                        send(&socket, address, self.config.scan_port, Outgoing::Scan).await;
                    }
                    scan.as_mut().reset(Instant::now() + scan_delay);
                    scan_delay = (scan_delay * 2).min(self.config.scan_interval());
                }
                () = &mut sweep => {
                    self.sweep(&socket).await;
                    sweep.as_mut().reset(Instant::now() + NEVER);
                }
                requested = changed(&mut discovery) => {
                    if requested {
                        info!("Govee discovery requested");
                        self.sweep(&socket).await;
                        scan_delay = FIRST_RESCAN;
                        scan.as_mut().reset(Instant::now());
                    } else {
                        discovery = None;
                    }
                }
                _ = poll.tick() => self.poll(ctx, &socket, &lamps).await,
                received = socket.recv_from(&mut buffer) => match received {
                    Ok((length, from)) => {
                        self.handle(ctx, &socket, &mut lamps, &mut known, &buffer[..length], from).await;
                    }
                    // Windows reports ICMP "port unreachable" as a receive error: not fatal.
                    Err(error) => debug!(%error, "receive failed"),
                },
                pending = ctx.next_command() => self.execute(&socket, &lamps, pending).await,
            }
        }
    }

    fn known(&self) -> KnownLamps {
        self.data_dir.as_deref().map(load_known).unwrap_or_default()
    }

    /// Asks every address of the local /24 directly, for lamps that ignore multicast.
    async fn sweep(&self, socket: &UdpSocket) {
        let Some(own) = local_address(&self.config).await else {
            debug!("no local IPv4 address: network sweep skipped");
            return;
        };
        let neighbours = neighbours(own);
        info!(around = %own, addresses = neighbours.len(), "Govee network sweep");
        for address in neighbours {
            send(socket, address, self.config.scan_port, Outgoing::Scan).await;
        }
    }

    async fn handle(
        &self,
        ctx: &DriverContext,
        socket: &UdpSocket,
        lamps: &mut BTreeMap<DeviceId, Lamp>,
        known: &mut KnownLamps,
        bytes: &[u8],
        from: SocketAddr,
    ) {
        match protocol::parse(bytes) {
            Some(Incoming::Scan(reply)) => {
                self.remember(known, &reply);
                self.on_scan(ctx, socket, lamps, reply).await;
            }
            Some(Incoming::Status(status)) => on_status(ctx, lamps, from.ip(), &status),
            None => debug!(%from, "ignored datagram"),
        }
    }

    fn remember(&self, known: &mut KnownLamps, reply: &ScanReply) {
        let Some(data_dir) = &self.data_dir else {
            return;
        };
        let native = protocol::native_id(&reply.device);
        if known.seen.get(&native) == Some(&reply.ip) {
            return;
        }
        known.seen.insert(native, reply.ip);
        if let Err(error) = save_known(data_dir, known) {
            warn!(%error, "cannot remember Govee lamp address");
        }
    }

    async fn on_scan(
        &self,
        ctx: &DriverContext,
        socket: &UdpSocket,
        lamps: &mut BTreeMap<DeviceId, Lamp>,
        reply: ScanReply,
    ) {
        let id = match ctx.device_id(&protocol::native_id(&reply.device)) {
            Ok(id) => id,
            Err(error) => {
                debug!(device = reply.device, %error, "unusable lamp id");
                return;
            }
        };
        if lamps.get(&id).is_some_and(|lamp| lamp.ip == reply.ip) {
            return;
        }
        info!(device = %id, ip = %reply.ip, sku = reply.sku, "lamp found");
        ctx.upsert_device(lamp_info(id.clone(), &reply.sku));
        lamps
            .entry(id)
            .and_modify(|lamp| lamp.ip = reply.ip)
            .or_insert(Lamp {
                ip: reply.ip,
                last_seen: None,
            });
        send(socket, reply.ip, self.config.control_port, Outgoing::Status).await;
    }

    async fn poll(
        &self,
        ctx: &DriverContext,
        socket: &UdpSocket,
        lamps: &BTreeMap<DeviceId, Lamp>,
    ) {
        let stale_after = self.config.poll_interval() * MISSED_POLLS;
        for (id, lamp) in lamps {
            if lamp
                .last_seen
                .is_none_or(|seen| seen.elapsed() >= stale_after)
            {
                ctx.set_available(id, false);
            }
            send(socket, lamp.ip, self.config.control_port, Outgoing::Status).await;
        }
    }

    async fn execute(
        &self,
        socket: &UdpSocket,
        lamps: &BTreeMap<DeviceId, Lamp>,
        pending: PendingCommand,
    ) {
        let command = pending.command();
        let Some(lamp) = lamps.get(&command.device) else {
            pending.fail("lamp not discovered yet");
            return;
        };
        let Some(message) = to_message(&command.point, &command.value) else {
            let reason = format!("point {:?} cannot be written", command.point);
            pending.fail(reason);
            return;
        };
        let ip = lamp.ip;
        if let Err(error) = socket
            .send_to(&message.encode(), (ip, self.config.control_port))
            .await
        {
            pending.fail(format!("send to {ip} failed: {error}"));
            return;
        }
        pending.succeed();
        send(socket, ip, self.config.control_port, Outgoing::Status).await;
    }
}

impl Driver for GoveeDriver {
    fn kind(&self) -> &'static str {
        "govee"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.serve(ctx))
    }
}

/// `true` for a discovery request, `false` once nobody can send any more.
async fn changed(requests: &mut Option<watch::Receiver<u64>>) -> bool {
    match requests {
        Some(receiver) => receiver.changed().await.is_ok(),
        None => std::future::pending().await,
    }
}

/// The address of the interface that reaches the rest of the network. Connecting a UDP
/// socket only selects a route: no packet is sent.
async fn local_address(config: &GoveeConfig) -> Option<Ipv4Addr> {
    if !config.bind.is_unspecified() {
        return Some(config.bind);
    }
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.ok()?;
    probe.connect((Ipv4Addr::new(1, 1, 1, 1), 53)).await.ok()?;
    match probe.local_addr().ok()?.ip() {
        IpAddr::V4(address) if address.is_private() => Some(address),
        _ => None,
    }
}

fn on_status(
    ctx: &DriverContext,
    lamps: &mut BTreeMap<DeviceId, Lamp>,
    from: IpAddr,
    status: &StatusReply,
) {
    let Some((id, lamp)) = lamps
        .iter_mut()
        .find(|(_, lamp)| IpAddr::V4(lamp.ip) == from)
    else {
        debug!(%from, "status from an unknown lamp");
        return;
    };
    lamp.last_seen = Some(Instant::now());
    for (point, value) in status_values(status) {
        ctx.set_state(id, point, value);
    }
    ctx.set_available(id, true);
}

fn status_values(status: &StatusReply) -> Vec<(&'static str, Value)> {
    let mut values = vec![
        ("on", Value::Bool(status.on_off == 1)),
        (
            "brightness",
            Value::Number(f64::from(status.brightness.min(100))),
        ),
    ];
    if status.color_temperature == 0 {
        values.push(("color_mode", Value::Text("color".to_owned())));
        values.push(("color", Value::Color(status.color)));
    } else {
        let kelvin = status.color_temperature.clamp(MIN_KELVIN, MAX_KELVIN);
        values.push(("color_mode", Value::Text("white".to_owned())));
        values.push(("color_temp", Value::Number(f64::from(kelvin))));
    }
    values
}

fn to_message(point: &str, value: &Value) -> Option<Outgoing> {
    match (point, value) {
        ("on", Value::Bool(on)) => Some(Outgoing::Turn(*on)),
        ("brightness", Value::Number(level)) if *level < 0.5 => Some(Outgoing::Turn(false)),
        ("brightness", Value::Number(level)) => Some(Outgoing::Brightness(percent(*level))),
        ("color", Value::Color(rgb)) => Some(Outgoing::Color(*rgb)),
        ("color_temp", Value::Number(kelvin)) => {
            Some(Outgoing::ColorTemperature(kelvin_of(*kelvin)))
        }
        _ => None,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn percent(level: f64) -> u8 {
    level.round().clamp(1.0, 100.0) as u8
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn kelvin_of(kelvin: f64) -> u32 {
    kelvin
        .round()
        .clamp(f64::from(MIN_KELVIN), f64::from(MAX_KELVIN)) as u32
}

fn lamp_info(id: DeviceId, sku: &str) -> DeviceInfo {
    DeviceInfo {
        id,
        name: format!("Govee {sku}"),
        manufacturer: Some("Govee".to_owned()),
        model: Some(sku.to_owned()),
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
            .writable()
            .with_unit(Unit::Percent),
            PointSpec::new("color", "Couleur", PointKind::Color, Semantic::Color).writable(),
            PointSpec::new(
                "color_temp",
                "Température de blanc",
                PointKind::Numeric {
                    min: f64::from(MIN_KELVIN),
                    max: f64::from(MAX_KELVIN),
                    step: 100.0,
                },
                Semantic::ColorTemperature,
            )
            .writable()
            .with_unit(Unit::Kelvin),
            PointSpec::new(
                "color_mode",
                "Mode",
                PointKind::Enum {
                    values: vec!["color".to_owned(), "white".to_owned()],
                },
                Semantic::ColorMode,
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{Outgoing, StatusReply, Value, status_values, to_message};
    use hestia_core::Rgb;

    #[test]
    fn white_status_reports_temperature() {
        let status = StatusReply {
            on_off: 1,
            brightness: 40,
            color: Rgb { r: 0, g: 0, b: 0 },
            color_temperature: 1500,
        };
        let values = status_values(&status);
        assert!(values.contains(&("color_mode", Value::Text("white".to_owned()))));
        assert!(values.contains(&("color_temp", Value::Number(2000.0))));
        assert!(!values.iter().any(|(point, _)| *point == "color"));
    }

    #[test]
    fn commands_map_to_lan_messages() {
        assert_eq!(
            to_message("on", &Value::Bool(false)),
            Some(Outgoing::Turn(false))
        );
        assert_eq!(
            to_message("brightness", &Value::Number(0.0)),
            Some(Outgoing::Turn(false))
        );
        assert_eq!(
            to_message("brightness", &Value::Number(39.6)),
            Some(Outgoing::Brightness(40))
        );
        assert_eq!(
            to_message("color_temp", &Value::Number(3000.0)),
            Some(Outgoing::ColorTemperature(3000))
        );
        assert_eq!(
            to_message("color_mode", &Value::Text("white".to_owned())),
            None
        );
    }
}
