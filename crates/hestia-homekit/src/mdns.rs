use std::collections::HashMap;

use anyhow::anyhow;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use tracing::warn;

const SERVICE: &str = "_hap._tcp.local.";

/// Announces the bridge as `_hap._tcp` so the Home app finds it.
pub(crate) struct Advertiser {
    daemon: ServiceDaemon,
    instance: String,
    host: String,
    port: u16,
}

impl Advertiser {
    pub(crate) fn new(name: &str, device_id: &str, port: u16) -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new().map_err(|error| anyhow!("cannot start mDNS: {error}"))?;
        // The TCP listener is IPv4 only: never advertise addresses it does not serve.
        if let Err(error) = daemon.disable_interface(IfKind::IPv6) {
            warn!(%error, "cannot restrict mDNS to IPv4");
        }
        let suffix: String = device_id
            .chars()
            .filter(char::is_ascii_hexdigit)
            .skip(8)
            .collect();
        Ok(Self {
            daemon,
            instance: name.to_owned(),
            host: format!("hestia-{}.local.", suffix.to_lowercase()),
            port,
        })
    }

    pub(crate) fn publish(&self, txt: HashMap<String, String>) {
        let registered = ServiceInfo::new(SERVICE, &self.instance, &self.host, "", self.port, txt)
            .map(ServiceInfo::enable_addr_auto)
            .and_then(|info| self.daemon.register(info));
        if let Err(error) = registered {
            warn!(%error, "HomeKit mDNS announcement failed");
        }
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}
