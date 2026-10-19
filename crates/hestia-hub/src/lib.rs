//! The hub owns device state and routes every command. Drivers only talk to it through
//! their [`DriverContext`]; consumers (API, `HomeKit`, MCP) only through [`Hub`].

mod driver;
mod event;
mod hub;
pub mod storage;
mod supervisor;

pub use driver::{BoxFuture, Driver, DriverContext, PendingCommand};
pub use event::{CommandOutcome, Event, now_millis};
pub use hub::{COMMAND_TIMEOUT, CommandError, DeviceSnapshot, Hub, Label, PointSnapshot};
pub use supervisor::spawn_driver;
