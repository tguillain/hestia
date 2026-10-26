use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::Context;
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};
use tracing::debug;

use crate::config::GoveeConfig;
use crate::protocol::{self, Incoming, Outgoing, ScanReply, StatusReply};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub scan: ScanReply,
    pub status: Option<StatusReply>,
}

/// One-shot, read-only discovery: lists the lamps answering within `wait` and their status.
///
/// # Errors
/// Fails when the listen port cannot be bound.
pub async fn discover(config: &GoveeConfig, wait: Duration) -> anyhow::Result<Vec<Discovered>> {
    let socket = bind(config).await?;
    send_scan(&socket, config).await;
    let mut found: BTreeMap<Ipv4Addr, Discovered> = BTreeMap::new();
    let deadline = Instant::now() + wait;
    let mut buffer = vec![0_u8; 2048];
    while let Ok(received) = timeout_at(deadline, socket.recv_from(&mut buffer)).await {
        let Ok((length, from)) = received else {
            continue;
        };
        match protocol::parse(&buffer[..length], from.ip()) {
            Some(Incoming::Scan(reply)) => {
                if let std::collections::btree_map::Entry::Vacant(e) = found.entry(reply.ip) {
                    send(&socket, reply.ip, config.control_port, Outgoing::Status).await;
                    e.insert(Discovered {
                        scan: reply,
                        status: None,
                    });
                }
            }
            Some(Incoming::Status(status)) => {
                if let IpAddr::V4(ip) = from.ip()
                    && let Some(lamp) = found.get_mut(&ip)
                {
                    lamp.status = Some(status);
                }
            }
            None => {}
        }
    }
    Ok(found.into_values().collect())
}

pub(crate) async fn bind(config: &GoveeConfig) -> anyhow::Result<UdpSocket> {
    UdpSocket::bind((config.bind, config.listen_port))
        .await
        .with_context(|| {
            format!(
                "cannot listen on UDP {}:{}",
                config.bind, config.listen_port
            )
        })
}

pub(crate) async fn send_scan(socket: &UdpSocket, config: &GoveeConfig) {
    let targets = config
        .discovery
        .then_some(protocol::MULTICAST)
        .into_iter()
        .chain(config.hosts.iter().copied());
    for ip in targets {
        send(socket, ip, config.scan_port, Outgoing::Scan).await;
    }
}

pub(crate) async fn send(socket: &UdpSocket, ip: Ipv4Addr, port: u16, message: Outgoing) {
    if let Err(error) = socket.send_to(&message.encode(), (ip, port)).await {
        debug!(%ip, port, %error, "send failed");
    }
}
