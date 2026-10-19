use std::path::Path;

use clap::Subcommand;
use hestia_api::{McpTokens, TokenScope};

#[derive(Subcommand)]
pub enum McpCommand {
    /// Manage the tokens AIs use to reach /mcp.
    #[command(subcommand)]
    Token(TokenCommand),
}

#[derive(Subcommand)]
pub enum TokenCommand {
    /// Create a token (shown once). Read-only unless --control is given.
    Add {
        name: String,
        /// Allow the AI to send commands, not only read.
        #[arg(long)]
        control: bool,
    },
    /// Revoke a token immediately.
    Remove { name: String },
    /// List tokens.
    List,
}

pub fn run(command: McpCommand, data_dir: &Path, public_url: Option<&str>) -> anyhow::Result<()> {
    let McpCommand::Token(command) = command;
    let tokens = McpTokens::open_in(data_dir)?;
    match command {
        TokenCommand::Add { name, control } => {
            let scope = if control {
                TokenScope::Control
            } else {
                TokenScope::Read
            };
            let token = tokens.create(&name, scope)?;
            let rights = if control {
                "lecture et commandes"
            } else {
                "lecture seule"
            };
            println!("Jeton « {name} » créé ({rights}). Il ne sera plus jamais affiché :");
            println!();
            println!("  {token}");
            println!();
            let url = public_url.map_or_else(
                || "http://<adresse-du-pi>:8080/mcp".to_owned(),
                |base| format!("{}/mcp", base.trim_end_matches('/')),
            );
            println!("Exemple pour Claude Code :");
            println!(
                "  claude mcp add --transport http hestia {url} --header \"Authorization: Bearer {token}\""
            );
        }
        TokenCommand::Remove { name } => {
            tokens.remove(&name)?;
            println!("Jeton « {name} » révoqué.");
        }
        TokenCommand::List => {
            for token in tokens.list() {
                let rights = match token.scope {
                    TokenScope::Read => "lecture seule",
                    TokenScope::Control => "lecture et commandes",
                };
                println!("{}  ({rights})", token.name);
            }
        }
    }
    Ok(())
}
