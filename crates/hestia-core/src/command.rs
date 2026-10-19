use serde::{Deserialize, Serialize};

use crate::device::DeviceId;
use crate::value::Value;

/// Who asked for a write. Always assigned by the server, never taken from the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Ui,
    Api,
    Mcp,
    #[serde(rename = "homekit")]
    HomeKit,
    Cli,
    Automation,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub device: DeviceId,
    pub point: String,
    pub value: Value,
}
