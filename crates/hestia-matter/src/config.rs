use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MatterConfig {
    /// Seconds between two state reads of each device.
    pub poll_interval_s: u64,
}

impl Default for MatterConfig {
    fn default() -> Self {
        Self { poll_interval_s: 5 }
    }
}

impl MatterConfig {
    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_s.max(1))
    }
}
