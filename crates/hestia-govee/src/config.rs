use std::net::Ipv4Addr;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoveeConfig {
    /// Multicast discovery on the LAN.
    pub discovery: bool,
    /// Lamps to query directly, for networks where multicast does not get through.
    pub hosts: Vec<Ipv4Addr>,
    pub bind: Ipv4Addr,
    pub listen_port: u16,
    pub scan_port: u16,
    pub control_port: u16,
    pub scan_interval_s: u64,
    pub poll_interval_s: u64,
}

impl Default for GoveeConfig {
    fn default() -> Self {
        Self {
            discovery: true,
            hosts: Vec::new(),
            bind: Ipv4Addr::UNSPECIFIED,
            listen_port: 4002,
            scan_port: 4001,
            control_port: 4003,
            scan_interval_s: 60,
            poll_interval_s: 5,
        }
    }
}

impl GoveeConfig {
    #[must_use]
    pub fn scan_interval(&self) -> Duration {
        Duration::from_secs(self.scan_interval_s.max(5))
    }

    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_s.max(1))
    }
}
