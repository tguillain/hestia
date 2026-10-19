//! Govee lights through the official LAN API: discovery by UDP multicast on port 4001,
//! replies on port 4002, control on port 4003. "LAN Control" must be enabled in the Govee app.

mod config;
mod discover;
mod driver;
mod known;
mod protocol;

pub use config::GoveeConfig;
pub use discover::{Discovered, discover};
pub use driver::GoveeDriver;
pub use known::{KnownLamps, add_manual_address, load_known, remove_manual_address};
pub use protocol::{ScanReply, StatusReply, native_id};
