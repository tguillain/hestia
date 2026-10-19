//! Health of the host itself. Points are declared only when the machine exposes them,
//! so the same driver works on a Pi and on any other Linux box.

mod probes;

use std::path::PathBuf;
use std::time::Duration;

use hestia_core::{DeviceId, DeviceInfo, PointKind, PointSpec, Semantic, Unit, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext};
use serde::Deserialize;
use tokio::time::{self, MissedTickBehavior};
use tracing::info;

use crate::probes::{
    disk_used, parse_cpu_times, parse_memory_used, parse_millidegrees, read, round1,
    under_voltage_alarm,
};

const THERMAL_ZONE: &str = "/sys/class/thermal/thermal_zone0/temp";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SystemConfig {
    pub poll_interval_s: u64,
    pub name: Option<String>,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            poll_interval_s: 30,
            name: None,
        }
    }
}

pub struct SystemDriver {
    config: SystemConfig,
    data_dir: PathBuf,
}

impl SystemDriver {
    /// `data_dir` selects the file system whose usage is reported.
    #[must_use]
    pub fn new(config: SystemConfig, data_dir: PathBuf) -> Self {
        Self { config, data_dir }
    }

    async fn serve(&self, ctx: &mut DriverContext) -> anyhow::Result<()> {
        if !cfg!(target_os = "linux") {
            info!("host metrics are only available on Linux");
            loop {
                ctx.next_command()
                    .await
                    .fail("system metrics are read-only");
            }
        }
        let id = ctx.device_id("host")?;
        ctx.upsert_device(self.info(id.clone()));
        ctx.set_available(&id, true);
        let mut previous_cpu = read("/proc/stat").as_deref().and_then(parse_cpu_times);
        let mut poll = time::interval(Duration::from_secs(self.config.poll_interval_s.max(5)));
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    let cpu = read("/proc/stat").as_deref().and_then(parse_cpu_times);
                    if let (Some((busy_then, total_then)), Some((busy, total))) = (previous_cpu, cpu)
                        && total > total_then
                    {
                        #[allow(clippy::cast_precision_loss)]
                        let load = (busy.saturating_sub(busy_then)) as f64 / (total - total_then) as f64;
                        ctx.set_state(&id, "cpu_load", Value::Number(round1(load * 100.0)));
                    }
                    previous_cpu = cpu.or(previous_cpu);
                    self.sample(ctx, &id);
                }
                pending = ctx.next_command() => pending.fail("system metrics are read-only"),
            }
        }
    }

    fn sample(&self, ctx: &DriverContext, id: &DeviceId) {
        if let Some(celsius) = read(THERMAL_ZONE).as_deref().and_then(parse_millidegrees) {
            ctx.set_state(id, "cpu_temperature", Value::Number(celsius));
        }
        if let Some(used) = read("/proc/meminfo").as_deref().and_then(parse_memory_used) {
            ctx.set_state(id, "memory", Value::Number(used));
        }
        if let Some(used) = disk_used(&self.data_dir) {
            ctx.set_state(id, "disk", Value::Number(used));
        }
        if let Some(alarm) = under_voltage_alarm() {
            ctx.set_state(id, "power_ok", Value::Bool(!alarm));
        }
    }

    fn info(&self, id: DeviceId) -> DeviceInfo {
        let percent = |key: &str, label: &str| {
            PointSpec::new(
                key,
                label,
                PointKind::Numeric {
                    min: 0.0,
                    max: 100.0,
                    step: 0.1,
                },
                Semantic::Other,
            )
            .with_unit(Unit::Percent)
        };
        let model = read("/proc/device-tree/model")
            .map(|model| model.trim_end_matches('\0').trim().to_owned())
            .filter(|model| !model.is_empty());
        let hostname = read("/proc/sys/kernel/hostname").map(|name| name.trim().to_owned());
        let mut points = vec![
            percent("cpu_load", "Charge CPU"),
            percent("memory", "Mémoire utilisée"),
            percent("disk", "Disque utilisé"),
        ];
        if read(THERMAL_ZONE).is_some() {
            points.insert(
                0,
                PointSpec::new(
                    "cpu_temperature",
                    "Température CPU",
                    PointKind::Numeric {
                        min: -40.0,
                        max: 150.0,
                        step: 0.1,
                    },
                    Semantic::Temperature,
                )
                .with_unit(Unit::Celsius),
            );
        }
        if under_voltage_alarm().is_some() {
            points.push(PointSpec::new(
                "power_ok",
                "Alimentation correcte",
                PointKind::Binary,
                Semantic::Other,
            ));
        }
        DeviceInfo {
            id,
            name: self
                .config
                .name
                .clone()
                .or(hostname)
                .unwrap_or_else(|| "Serveur".to_owned()),
            manufacturer: None,
            model,
            points,
        }
    }
}

impl Driver for SystemDriver {
    fn kind(&self) -> &'static str {
        "system"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.serve(ctx))
    }
}
