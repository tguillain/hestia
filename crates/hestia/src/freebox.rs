use std::time::Duration;

use anyhow::bail;
use clap::Subcommand;
use hestia_freebox::{FreeboxApi, FreeboxConfig, Token, load_token, save_token};

use crate::config::{Config, DriverKind};

const AUTHORIZATION_WAIT: Duration = Duration::from_secs(130);
const AUTHORIZATION_POLL: Duration = Duration::from_secs(2);

#[derive(Subcommand)]
pub enum FreeboxCommand {
    /// Ask the Freebox to authorise hestia (confirm on the box).
    Register,
    /// Show the box model and whether hestia is authorised.
    Status,
}

pub async fn run(command: FreeboxCommand, config: &Config) -> anyhow::Result<()> {
    let settings: FreeboxConfig = config
        .settings_of(DriverKind::Freebox)
        .transpose()?
        .unwrap_or_default();
    let (api, version) = FreeboxApi::discover(&settings.host, settings.port).await?;
    let token = load_token(&config.data_dir)?;
    match command {
        FreeboxCommand::Status => {
            println!("{} (API {})", version.box_model_name, version.api_version);
            match token {
                Some(token) if token.box_uid == version.uid => {
                    println!("Hestia est autorisé sur cette box.");
                }
                Some(_) => println!(
                    "Le jeton enregistré appartient à une autre box : relance `hestia freebox register`."
                ),
                None => println!("Pas encore autorisé : lance `hestia freebox register`."),
            }
        }
        FreeboxCommand::Register => {
            let (app_token, track_id) = api.request_authorization(&device_name()).await?;
            println!("Demande envoyée à la {}.", version.box_model_name);
            println!(
                "Valide l'application « Hestia » sur la Freebox (écran de la box ou appli Freebox) dans les 2 minutes…"
            );
            let started = tokio::time::Instant::now();
            loop {
                match api.authorization_status(track_id).await?.as_str() {
                    "granted" => break,
                    "pending" if started.elapsed() < AUTHORIZATION_WAIT => {
                        tokio::time::sleep(AUTHORIZATION_POLL).await;
                    }
                    "denied" => bail!("demande refusée sur la Freebox"),
                    status => bail!("demande non validée ({status})"),
                }
            }
            save_token(
                &config.data_dir,
                &Token {
                    app_token,
                    box_uid: version.uid,
                },
            )?;
            println!("Hestia est autorisé. Le service s'y connectera automatiquement.");
            println!(
                "Pour piloter le Wi-Fi, active la permission « Modification des réglages de la Freebox » \
                 pour Hestia dans Freebox OS (Paramètres > Gestion des accès > Applications)."
            );
        }
    }
    Ok(())
}

fn device_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "hestia".to_owned())
}
