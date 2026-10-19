use std::time::{SystemTime, UNIX_EPOCH};

use hestia_core::{DeviceId, Origin, Value};
use serde::Serialize;

use crate::hub::CommandError;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    DeviceUpdated {
        device: DeviceId,
    },
    DeviceRemoved {
        device: DeviceId,
    },
    State {
        device: DeviceId,
        point: String,
        value: Value,
        at: i64,
    },
    Availability {
        device: DeviceId,
        available: bool,
    },
    Command {
        device: DeviceId,
        point: String,
        value: Value,
        origin: Origin,
        outcome: CommandOutcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum CommandOutcome {
    Done,
    Failed(String),
}

impl CommandOutcome {
    pub(crate) fn of(result: &Result<(), CommandError>) -> Self {
        match result {
            Ok(()) => Self::Done,
            Err(error) => Self::Failed(error.to_string()),
        }
    }
}

/// Wall-clock time in Unix milliseconds.
#[must_use]
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
