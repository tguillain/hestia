use std::net::Ipv4Addr;
use std::time::Duration;

use clap::Subcommand;
use hestia_govee::{Discovered, GoveeConfig, StatusReply, native_id};

use crate::config::{Config, DriverKind};

#[derive(Subcommand)]
pub enum GoveeCommand {
    /// List the lamps answering on the LAN (read-only).
    Scan {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        /// Lamp IP to query directly, in addition to multicast discovery.
        #[arg(long = "host")]
        hosts: Vec<Ipv4Addr>,
    },
}

pub async fn run(command: GoveeCommand, config: Option<&Config>) -> anyhow::Result<()> {
    let GoveeCommand::Scan { seconds, hosts } = command;
    let mut settings: GoveeConfig = config
        .and_then(|config| config.settings_of::<GoveeConfig>(DriverKind::Govee))
        .transpose()?
        .unwrap_or_default();
    settings.hosts.extend(hosts);
    let wait = Duration::from_secs(seconds);
    let lamps = hestia_govee::discover(&settings, wait).await?;
    if lamps.is_empty() {
        println!("Aucune lampe Govee n'a répondu en {} s.", wait.as_secs());
        println!(
            "Vérifie que « Contrôle LAN » est activé dans l'app Govee et que le pare-feu laisse passer l'UDP 4002."
        );
        return Ok(());
    }
    let labels = config.map(Config::labels).unwrap_or_default();
    for Discovered { scan, status } in lamps {
        let id = format!("govee:{}", native_id(&scan.device));
        let name = id
            .parse()
            .ok()
            .and_then(|id| labels.get(&id))
            .and_then(|label| label.name.clone())
            .unwrap_or_default();
        let state = status
            .as_ref()
            .map_or_else(|| "pas de statut".to_owned(), describe);
        println!(
            "{id}  {:<15}  {:<6}  {name:<12}  {state}",
            scan.ip.to_string(),
            scan.sku
        );
    }
    Ok(())
}

fn describe(status: &StatusReply) -> String {
    let mode = if status.color_temperature == 0 {
        format!(
            "couleur #{:02x}{:02x}{:02x}",
            status.color.r, status.color.g, status.color.b
        )
    } else {
        format!("blanc {} K", status.color_temperature)
    };
    let power = if status.on_off == 1 {
        "allumée"
    } else {
        "éteinte"
    };
    format!("{power}, {} %, {mode}", status.brightness)
}
