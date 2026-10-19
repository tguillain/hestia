use std::io::{BufRead, IsTerminal};
use std::path::Path;

use anyhow::{Context, bail};
use clap::Subcommand;
use hestia_api::{UserStore, hash_password};

const MIN_PASSWORD_CHARS: usize = 10;

#[derive(Subcommand)]
pub enum UserCommand {
    /// Create an account. The password is read from the terminal (or stdin when piped).
    Add { name: String },
    /// Change an account's password.
    Passwd { name: String },
    /// Delete an account; its sessions stop working immediately.
    Remove { name: String },
    /// List accounts.
    List,
}

pub fn run(command: UserCommand, data_dir: &Path) -> anyhow::Result<()> {
    let store = UserStore::open_in(data_dir)?;
    match command {
        UserCommand::Add { name } => {
            let name = name.to_lowercase();
            if store.contains(&name) {
                bail!("l'utilisateur {name} existe déjà");
            }
            store.add(&name, hash_password(&read_new_password()?)?)?;
            println!("Compte {name} créé.");
        }
        UserCommand::Passwd { name } => {
            if !store.contains(&name) {
                bail!("utilisateur inconnu : {name}");
            }
            store.set_password(&name, hash_password(&read_new_password()?)?)?;
            println!("Mot de passe de {name} changé.");
        }
        UserCommand::Remove { name } => {
            store.remove(&name)?;
            println!("Compte {name} supprimé.");
        }
        UserCommand::List => {
            for name in store.names() {
                println!("{name}");
            }
        }
    }
    Ok(())
}

fn read_new_password() -> anyhow::Result<String> {
    let password = if std::io::stdin().is_terminal() {
        let first = rpassword::prompt_password("Mot de passe : ")?;
        let second = rpassword::prompt_password("Confirmation : ")?;
        if first != second {
            bail!("les deux saisies sont différentes");
        }
        first
    } else {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("cannot read password from stdin")?;
        line.trim_end_matches(['\r', '\n']).to_owned()
    };
    if password.chars().count() < MIN_PASSWORD_CHARS {
        bail!("le mot de passe doit faire au moins {MIN_PASSWORD_CHARS} caractères");
    }
    Ok(password)
}
