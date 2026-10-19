//! Is a machine on the LAN up? Uses the system `ping`, which needs no privileges
//! (raw ICMP sockets would require root or capabilities).

use std::process::Stdio;
use std::time::Duration;

use anyhow::bail;
use hestia_core::{DeviceId, DeviceInfo, PointKind, PointSpec, Semantic, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext};
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::{self, MissedTickBehavior};

/// A host is reported down only after this many failed pings in a row.
const FAILURES_BEFORE_DOWN: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PingConfig {
    pub hosts: Vec<HostConfig>,
    pub poll_interval_s: u64,
    pub timeout_ms: u64,
}

impl Default for PingConfig {
    fn default() -> Self {
        Self {
            hosts: Vec::new(),
            poll_interval_s: 30,
            timeout_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Stable identifier, e.g. `pc`: the device becomes `<driver>:pc`.
    pub id: String,
    pub host: String,
    pub name: Option<String>,
}

impl PingConfig {
    /// # Errors
    /// Refuses host names that could be read as command-line options.
    pub fn validate(&self) -> anyhow::Result<()> {
        for host in &self.hosts {
            let safe = !host.host.is_empty()
                && !host.host.starts_with('-')
                && host
                    .host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
            if !safe {
                bail!("invalid host {:?}", host.host);
            }
        }
        Ok(())
    }
}

pub struct PingDriver {
    config: PingConfig,
}

impl PingDriver {
    #[must_use]
    pub fn new(config: PingConfig) -> Self {
        Self { config }
    }

    async fn serve(&self, ctx: &mut DriverContext) -> anyhow::Result<()> {
        self.config.validate()?;
        let mut hosts = Vec::new();
        for host in &self.config.hosts {
            let id = ctx.device_id(&host.id)?;
            ctx.upsert_device(host_info(id.clone(), host));
            ctx.set_available(&id, true);
            hosts.push((id, host, 0_u32));
        }
        let timeout = Duration::from_millis(self.config.timeout_ms.clamp(200, 10_000));
        let mut poll = time::interval(Duration::from_secs(self.config.poll_interval_s.max(5)));
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    for (id, host, failures) in &mut hosts {
                        if ping(&host.host, timeout).await {
                            *failures = 0;
                            ctx.set_state(id, "online", Value::Bool(true));
                        } else {
                            *failures += 1;
                            if *failures >= FAILURES_BEFORE_DOWN {
                                ctx.set_state(id, "online", Value::Bool(false));
                            }
                        }
                    }
                }
                pending = ctx.next_command() => pending.fail("presence is read-only"),
            }
        }
    }
}

impl Driver for PingDriver {
    fn kind(&self) -> &'static str {
        "ping"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.serve(ctx))
    }
}

/// One echo request through the system tool.
pub async fn ping(host: &str, timeout: Duration) -> bool {
    let mut command = Command::new("ping");
    if cfg!(windows) {
        command.args(["-n", "1", "-w", &timeout.as_millis().to_string(), host]);
    } else {
        let seconds = timeout.as_secs().max(1).to_string();
        command.args(["-c", "1", "-W", &seconds, host]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(Ok(output)) = time::timeout(timeout + Duration::from_secs(2), command.output()).await
    else {
        return false;
    };
    // Windows also exits with 0 for "destination unreachable" relayed by the router:
    // only a reply carrying a TTL counts.
    output.status.success()
        && (!cfg!(windows)
            || String::from_utf8_lossy(&output.stdout)
                .to_ascii_uppercase()
                .contains("TTL="))
}

fn host_info(id: DeviceId, host: &HostConfig) -> DeviceInfo {
    DeviceInfo {
        id,
        name: host.name.clone().unwrap_or_else(|| host.host.clone()),
        manufacturer: None,
        model: Some(host.host.clone()),
        points: vec![PointSpec::new(
            "online",
            "En ligne",
            PointKind::Binary,
            Semantic::Connectivity,
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::{HostConfig, PingConfig, ping};
    use std::time::Duration;

    #[test]
    fn option_like_hosts_are_refused() {
        let config = |host: &str| PingConfig {
            hosts: vec![HostConfig {
                id: "pc".to_owned(),
                host: host.to_owned(),
                name: None,
            }],
            ..PingConfig::default()
        };
        assert!(config("192.168.1.50").validate().is_ok());
        assert!(config("pc-salon.local").validate().is_ok());
        assert!(config("-f").validate().is_err());
        assert!(config("1.2.3.4; reboot").validate().is_err());
    }

    #[tokio::test]
    async fn loopback_answers() {
        assert!(ping("127.0.0.1", Duration::from_secs(1)).await);
    }
}
