mod config;
mod freebox;
mod govee;
mod homekit;
mod mcp;
mod release;
mod serve;
mod users;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::freebox::FreeboxCommand;
use crate::govee::GoveeCommand;
use crate::homekit::HomeKitCommand;
use crate::mcp::McpCommand;
use crate::release::ReleaseCommand;
use crate::users::UserCommand;

/// `0.1.0+20261019.2045` for builds made by `deploy/build.sh`, plain `0.1.0` otherwise.
pub const VERSION: &str = match option_env!("HESTIA_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(name = "hestia", version = VERSION, about = "Hestia, hub domotique léger")]
struct Cli {
    /// Configuration file.
    #[arg(long, short, global = true, default_value = "hestia.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the hub and the dashboard.
    Serve,
    /// Check the configuration and exit.
    CheckConfig,
    /// Manage dashboard accounts.
    #[command(subcommand)]
    User(UserCommand),
    /// Apple Home bridge.
    #[command(subcommand)]
    Homekit(HomeKitCommand),
    /// Access for AIs (Model Context Protocol).
    #[command(subcommand)]
    Mcp(McpCommand),
    /// Freebox authorisation.
    #[command(subcommand)]
    Freebox(FreeboxCommand),
    /// Govee tools.
    #[command(subcommand)]
    Govee(GoveeCommand),
    /// Release signing, on the build machine.
    #[command(subcommand)]
    Release(ReleaseCommand),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Commands::Release(command) => release::run(command),
        Commands::Serve => serve::run(Config::load(&cli.config)?).await,
        Commands::CheckConfig => {
            let config = Config::load(&cli.config)?;
            println!(
                "{} OK: {} driver(s), {} labelled device(s), dashboard on {}",
                cli.config.display(),
                config.drivers.len(),
                config.devices.len(),
                config.http.listen
            );
            Ok(())
        }
        Commands::User(command) => users::run(command, &Config::load(&cli.config)?.data_dir),
        Commands::Mcp(command) => {
            let config = Config::load(&cli.config)?;
            mcp::run(command, &config.data_dir, config.http.public_url.as_deref())
        }
        Commands::Freebox(command) => freebox::run(command, &Config::load(&cli.config)?).await,
        Commands::Homekit(command) => homekit::run(command, &Config::load(&cli.config)?.data_dir),
        Commands::Govee(command) => {
            let config = if cli.config.exists() {
                Some(Config::load(&cli.config)?)
            } else {
                None
            };
            govee::run(command, config.as_ref()).await
        }
    }
}
