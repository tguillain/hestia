use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Subcommand;
use hestia_api::release::{from_hex, generate_key, sign, to_hex, verify};

#[derive(Subcommand)]
pub enum ReleaseCommand {
    /// Create the release key, once, on the build machine; prints its public half.
    Keygen {
        #[arg(long)]
        key: Option<PathBuf>,
    },
    /// Sign a program for the Settings page: writes `<program>.sig`.
    Sign {
        program: PathBuf,
        #[arg(long)]
        key: Option<PathBuf>,
    },
}

pub fn run(command: ReleaseCommand) -> anyhow::Result<()> {
    match command {
        ReleaseCommand::Keygen { key } => {
            let path = key.unwrap_or_else(default_key);
            if path.exists() {
                bail!(
                    "{} existe déjà : une clé ne se remplace pas",
                    path.display()
                );
            }
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder)?;
            }
            let (seed, public) = generate_key();
            std::fs::write(&path, to_hex(&seed))?;
            println!(
                "Clé privée : {} (à garder secrète, et à sauvegarder)",
                path.display()
            );
            println!("Clé publique, à mettre dans crates/hestia-api/release.pub :");
            println!("{}", to_hex(&public));
        }
        ReleaseCommand::Sign { program, key } => {
            let path = key.unwrap_or_else(default_key);
            let seed = read_seed(&path)?;
            let bytes = std::fs::read(&program)
                .with_context(|| format!("cannot read {}", program.display()))?;
            let signature = sign(&seed, &bytes);
            if !verify(&bytes, &signature) {
                bail!("cette clé ne correspond pas à la clé publique compilée dans ce programme");
            }
            let output = signature_path(&program);
            std::fs::write(&output, format!("{}\n", to_hex(&signature)))?;
            println!("Signature écrite dans {}", output.display());
        }
    }
    Ok(())
}

fn default_key() -> PathBuf {
    if let Some(path) = std::env::var_os("HESTIA_RELEASE_KEY") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".hestia").join("release.key")
}

fn read_seed(path: &Path) -> anyhow::Result<[u8; 32]> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "clé introuvable : {} (lance `hestia release keygen`)",
            path.display()
        )
    })?;
    from_hex(text.trim())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .context("clé de publication invalide")
}

fn signature_path(program: &Path) -> PathBuf {
    let mut name = program.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}
