//! Matter lights on the local network. Hestia runs its own Matter fabric: a device already
//! in another ecosystem (the maker's app, Apple Home…) is shared with it through a pairing
//! code, and keeps working there too.

mod config;
mod driver;
mod light;
mod mdns;
mod stack;
mod store;

pub use config::MatterConfig;
pub use driver::{
    MatterDriver, PairedSummary, Pairing, PairingRequests, paired_devices, pairing_channel,
};
pub use stack::{MATTER_PORT, SetupCode};
