//! Minecraft Java servers: online state, players, latency, version and message of the day,
//! read with the same status ping the game's server list uses.

mod driver;
mod protocol;

pub use driver::{MinecraftConfig, MinecraftDriver, ServerConfig};
pub use protocol::{Status, query};
