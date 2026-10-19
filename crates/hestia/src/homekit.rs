use std::path::Path;

use anyhow::bail;
use clap::Subcommand;
use hestia_core::DeviceId;

#[derive(Subcommand)]
pub enum HomeKitCommand {
    /// Show the setup code and pairing state.
    Status,
    /// Forget the bridge identity and every pairing (stop the service first).
    Reset {
        /// Confirm the reset: every iPhone will have to pair again.
        #[arg(long)]
        yes: bool,
    },
    /// Remove a light from Apple Home (stop the service first).
    Forget { device: DeviceId },
}

pub fn run(command: HomeKitCommand, data_dir: &Path) -> anyhow::Result<()> {
    match command {
        HomeKitCommand::Status => {
            let status = hestia_homekit::offline_status(data_dir)?;
            if status.paired {
                println!("Appairé avec {} appareil(s) Apple.", status.controllers);
            } else {
                println!("Pas encore appairé.");
                println!("Code d'appairage : {}", status.setup_code);
                println!("Charge utile QR  : {}", status.setup_uri);
            }
            println!("Lampes exposées  : {}", status.accessories);
        }
        HomeKitCommand::Reset { yes } => {
            if !yes {
                bail!("ajoute --yes pour confirmer : tous les iPhone devront ré-appairer le pont");
            }
            hestia_homekit::reset(data_dir)?;
            println!(
                "Pont HomeKit réinitialisé : nouvelle identité et nouveau code au prochain démarrage."
            );
        }
        HomeKitCommand::Forget { device } => {
            if hestia_homekit::forget(data_dir, &device)? {
                println!("{device} retiré d'Apple Home.");
            } else {
                println!("{device} n'était pas exposé.");
            }
        }
    }
    Ok(())
}
