use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use hestia_api::ChartConfig;
use hestia_core::DeviceId;
use hestia_freebox::{FreeboxConfig, FreeboxDriver};
use hestia_govee::{GoveeConfig, GoveeDriver};
use hestia_history::HistoryConfig;
use hestia_homekit::HomeKitConfig;
use hestia_hub::{Driver, Label};
use hestia_minecraft::{MinecraftConfig, MinecraftDriver};
use hestia_ping::{PingConfig, PingDriver};
use hestia_system::{SystemConfig, SystemDriver};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::watch;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Users, sessions and history. Relative paths start from the config file's folder.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub homekit: HomeKitConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    /// Graphs shown on the dashboard.
    #[serde(default, rename = "chart")]
    pub charts: Vec<ChartConfig>,
    #[serde(default, rename = "driver")]
    pub drivers: Vec<DriverConfig>,
    #[serde(default, rename = "device")]
    pub devices: Vec<DeviceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Address the household uses from outside, e.g. <https://ha.example.fr> (shown in hints).
    #[serde(default)]
    pub public_url: Option<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            public_url: None,
        }
    }
}

/// Where the Settings page and the MCP tools look for new versions.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfig {
    /// GitHub `owner/repo` publishing signed releases; empty to disable.
    #[serde(default = "default_update_repo")]
    pub github: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            github: default_update_repo(),
        }
    }
}

fn default_update_repo() -> String {
    "tguillain/hestia".to_owned()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8080))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverConfig {
    pub id: String,
    pub kind: DriverKind,
    #[serde(default)]
    pub settings: toml::Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Govee,
    Minecraft,
    Ping,
    System,
    Freebox,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    pub id: DeviceId,
    pub name: Option<String>,
    pub room: Option<String>,
}

impl Config {
    /// # Errors
    /// Fails when the file is unreadable, malformed or inconsistent.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let mut config = Self::parse(&raw)
            .with_context(|| format!("invalid configuration {}", path.display()))?;
        if config.data_dir.is_relative()
            && let Some(folder) = path.parent()
        {
            config.data_dir = folder.join(&config.data_dir);
        }
        Ok(config)
    }

    fn parse(raw: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(raw)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let mut ids = HashSet::new();
        for driver in &self.drivers {
            let valid = !driver.id.is_empty()
                && driver
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
            if !valid {
                bail!(
                    "driver id {:?} must use only a-z, 0-9, '-' and '_'",
                    driver.id
                );
            }
            if !ids.insert(driver.id.as_str()) {
                bail!("driver id {:?} is declared twice", driver.id);
            }
            driver.build(&self.data_dir, &watch::channel(0).1)?;
        }
        let mut devices = HashSet::new();
        for device in &self.devices {
            if !devices.insert(&device.id) {
                bail!("device {} is declared twice", device.id);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn labels(&self) -> HashMap<DeviceId, Label> {
        self.devices
            .iter()
            .map(|device| {
                let label = Label {
                    name: device.name.clone(),
                    room: device.room.clone(),
                };
                (device.id.clone(), label)
            })
            .collect()
    }

    /// Settings of the first driver of `kind`, for the command-line tools.
    pub fn settings_of<T: DeserializeOwned>(&self, kind: DriverKind) -> Option<anyhow::Result<T>> {
        self.drivers
            .iter()
            .find(|driver| driver.kind == kind)
            .map(DriverConfig::settings)
    }
}

impl DriverConfig {
    /// # Errors
    /// Fails when the driver settings do not match its kind.
    pub fn build(
        &self,
        data_dir: &Path,
        discovery: &watch::Receiver<u64>,
    ) -> anyhow::Result<Arc<dyn Driver>> {
        Ok(match self.kind {
            DriverKind::Govee => Arc::new(
                GoveeDriver::new(self.settings::<GoveeConfig>()?)
                    .with_memory(data_dir.to_path_buf())
                    .with_discovery(discovery.clone()),
            ),
            DriverKind::Minecraft => {
                Arc::new(MinecraftDriver::new(self.settings::<MinecraftConfig>()?))
            }
            DriverKind::Ping => {
                let settings: PingConfig = self.settings()?;
                settings.validate()?;
                Arc::new(PingDriver::new(settings))
            }
            DriverKind::System => Arc::new(SystemDriver::new(
                self.settings::<SystemConfig>()?,
                data_dir.to_path_buf(),
            )),
            DriverKind::Freebox => Arc::new(FreeboxDriver::new(
                self.settings::<FreeboxConfig>()?,
                data_dir.to_path_buf(),
            )),
        })
    }

    fn settings<T: DeserializeOwned>(&self) -> anyhow::Result<T> {
        toml::Value::Table(self.settings.clone())
            .try_into()
            .with_context(|| format!("invalid settings for driver {:?}", self.id))
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn example_config_is_valid() {
        let config = Config::parse(include_str!("../../../deploy/hestia.example.toml")).unwrap();
        assert_eq!(config.drivers.len(), 5);
        assert_eq!(config.labels().len(), 3);
    }

    #[test]
    fn rejects_duplicates_and_bad_settings() {
        let duplicated = "[[driver]]\nid = \"govee\"\nkind = \"govee\"\n[[driver]]\nid = \"govee\"\nkind = \"govee\"\n";
        assert!(Config::parse(duplicated).is_err());
        let bad_setting =
            "[[driver]]\nid = \"govee\"\nkind = \"govee\"\n[driver.settings]\nunknown = 1\n";
        assert!(Config::parse(bad_setting).is_err());
        let bad_id = "[[driver]]\nid = \"Govee:1\"\nkind = \"govee\"\n";
        assert!(Config::parse(bad_id).is_err());
    }
}
