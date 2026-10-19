//! Answers rs-matter's discovery requests with `mdns-sd`, the library the `HomeKit` bridge
//! already uses, rather than rs-matter's own responder.

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::time::{Duration, Instant};

use embassy_futures::select::{Either, select};
use mdns_sd::{ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent};
use rs_matter::Matter;
use rs_matter::transport::network::MatterRemoteService;
use rs_matter::transport::network::mdns::{DottedName, MdnsRemoteService, score_ip_address};
use tracing::debug;

const OPERATIONAL: &str = "_matter._tcp.local.";
const COMMISSIONABLE: &str = "_matterc._udp.local.";
/// rs-matter gives up on a resolve after 5 s; answers arriving later are useless.
const QUERY_TIME: Duration = Duration::from_secs(6);
const CHECK_EVERY: Duration = Duration::from_millis(200);

/// Last known address of each node, tried before mDNS: devices on a busy Wi-Fi often
/// miss queries, and a stable lamp rarely changes address.
pub(crate) type AddressBook = RefCell<HashMap<u64, SocketAddr>>;

/// Never returns: serves every resolve and browse request of `matter`.
pub(crate) async fn serve(matter: &Matter<'_>, daemon: &ServiceDaemon, book: &AddressBook) {
    let transport = matter.transport();
    loop {
        match select(
            transport.wait_mdns_resolve_request(),
            transport.wait_mdns_browse_request(),
        )
        .await
        {
            Either::First(service) => resolve(matter, daemon, book, &service).await,
            Either::Second(_) => browse(matter, daemon).await,
        }
    }
}

async fn resolve(
    matter: &Matter<'_>,
    daemon: &ServiceDaemon,
    book: &AddressBook,
    service: &MatterRemoteService,
) {
    let MatterRemoteService::Operational { node_id, .. } = service else {
        return;
    };
    let mut name = heapless::String::<128>::new();
    service.instance_name(&mut name);
    let known = book.borrow().get(node_id).copied();
    if let Some(address) = known {
        debug!(node = format!("{node_id:016x}"), %address, "resolved from memory");
        deposit_resolve(
            matter,
            &name,
            address.ip(),
            address.port(),
            &[],
            scope_of(&address),
        );
        return;
    }
    let mut best: Option<(u8, SocketAddr)> = None;
    query(
        daemon,
        OPERATIONAL,
        || matter.transport().mdns_resolve_in_flight(),
        |found| {
            if !found.fullname.eq_ignore_ascii_case(&format!("{name}.")) {
                return;
            }
            let txt: Vec<(&str, &str)> = found
                .txt_properties
                .iter()
                .map(|property| (property.key(), property.val_str()))
                .collect();
            for (ip, scope) in addresses(found) {
                deposit_resolve(
                    matter,
                    trimmed(&found.fullname),
                    ip,
                    found.port,
                    &txt,
                    scope,
                );
                let score = score_ip_address(&ip);
                if best.is_none_or(|(kept, _)| score > kept) {
                    let address = match ip {
                        IpAddr::V6(v6) => {
                            SocketAddr::V6(SocketAddrV6::new(v6, found.port, 0, scope))
                        }
                        IpAddr::V4(v4) => SocketAddr::from((v4, found.port)),
                    };
                    best = Some((score, address));
                }
            }
        },
    )
    .await;
    // Remembered so the driver can store it and try it first next time.
    if let Some((_, address)) = best {
        book.borrow_mut().insert(*node_id, address);
    }
}

async fn browse(matter: &Matter<'_>, daemon: &ServiceDaemon) {
    query(
        daemon,
        COMMISSIONABLE,
        || matter.transport().mdns_browse_in_flight(),
        |found| {
            let txt: Vec<(&str, &str)> = found
                .txt_properties
                .iter()
                .map(|property| (property.key(), property.val_str()))
                .collect();
            for (ip, scope) in addresses(found) {
                matter
                    .transport()
                    .try_deposit_mdns_browse(&MdnsRemoteService {
                        instance_name: DottedName(trimmed(&found.fullname)),
                        port: Some(found.port),
                        addrs: std::iter::once(ip),
                        txt: txt.iter().copied(),
                        scope_id: scope,
                    });
            }
        },
    )
    .await;
}

/// Browses `service_type` while rs-matter still waits for an answer.
async fn query(
    daemon: &ServiceDaemon,
    service_type: &str,
    in_flight: impl Fn() -> bool,
    mut on_found: impl FnMut(&ResolvedService),
) {
    let events = match daemon.browse(service_type) {
        Ok(events) => events,
        Err(error) => {
            debug!(%error, service_type, "mDNS browse failed");
            return;
        }
    };
    let deadline = Instant::now() + QUERY_TIME;
    while in_flight() && Instant::now() < deadline {
        let next = select(events.recv_async(), async_io::Timer::after(CHECK_EVERY)).await;
        match next {
            Either::First(Ok(ServiceEvent::ServiceResolved(found))) => on_found(&found),
            Either::First(Ok(_)) | Either::Second(_) => {}
            Either::First(Err(_)) => break,
        }
    }
    let _ = daemon.stop_browse(service_type);
}

fn deposit_resolve(
    matter: &Matter<'_>,
    name: &str,
    ip: IpAddr,
    port: u16,
    txt: &[(&str, &str)],
    scope: u32,
) {
    matter.transport().try_deposit_mdns_resolve(
        &MdnsRemoteService {
            instance_name: DottedName(name),
            port: Some(port),
            addrs: std::iter::once(ip),
            txt: txt.iter().copied(),
            scope_id: scope,
        },
        &[],
    );
}

/// The Matter socket is IPv6: IPv4 addresses travel as IPv4-mapped ones.
fn addresses(found: &ResolvedService) -> Vec<(IpAddr, u32)> {
    found
        .addresses
        .iter()
        .map(|address| match address {
            ScopedIp::V6(v6) => (IpAddr::V6(*v6.addr()), v6.scope_id().index),
            other => (mapped(other.to_ip_addr()), 0),
        })
        .collect()
}

pub(crate) fn mapped(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V6(v4.to_ipv6_mapped()),
        v6 @ IpAddr::V6(_) => v6,
    }
}

fn scope_of(address: &SocketAddr) -> u32 {
    match address {
        SocketAddr::V6(v6) => v6.scope_id(),
        SocketAddr::V4(_) => 0,
    }
}

fn trimmed(name: &str) -> &str {
    name.strip_suffix('.').unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::{mapped, trimmed};
    use std::net::IpAddr;

    #[test]
    fn names_and_addresses_are_normalised() {
        assert_eq!(
            trimmed("AB-CD._matter._tcp.local."),
            "AB-CD._matter._tcp.local"
        );
        let v4: IpAddr = "192.168.1.40".parse().unwrap();
        assert_eq!(mapped(v4), "::ffff:192.168.1.40".parse::<IpAddr>().unwrap());
        let v6: IpAddr = "fe80::1".parse().unwrap();
        assert_eq!(mapped(v6), v6);
    }
}
