//! Small JSON state files (accounts, sessions, `HomeKit` pairings).

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Reads a JSON file, or returns the default when it does not exist yet.
///
/// # Errors
/// Fails when the file exists but cannot be read or parsed.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> anyhow::Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("cannot parse {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes through a temporary file and a rename, so a power cut never leaves a torn file.
///
/// # Errors
/// Fails when the folder or the file cannot be written.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temporary)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))
}
