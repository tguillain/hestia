use std::path::{Path, PathBuf};

use hestia_hub::storage::{load_json, save_json};
use serde::{Deserialize, Serialize};

/// The app token is a long-lived secret: whoever holds it controls the Freebox
/// within the permissions granted to the application.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub app_token: String,
    /// `uid` of the Freebox that granted the token.
    pub box_uid: String,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("freebox.json")
}

/// # Errors
/// Fails when the file exists but cannot be read.
pub fn load_token(data_dir: &Path) -> anyhow::Result<Option<Token>> {
    let token: Token = load_json(&path(data_dir))?;
    Ok((!token.app_token.is_empty()).then_some(token))
}

/// # Errors
/// Fails when the file cannot be written.
pub fn save_token(data_dir: &Path, token: &Token) -> anyhow::Result<()> {
    let path = path(data_dir);
    save_json(&path, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
