//! Updating the program from the Settings page. Releases live in `<data>/releases/`:
//! `hestia` is started instead of the installed program when present, `hestia.previous`
//! is kept for rolling back.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use serde_json::{Value as Json_, json};
use tracing::{info, warn};

use super::CurrentUser;
use crate::AppState;
use crate::error::ApiError;
use crate::release;

const CURRENT: &str = "hestia";
const PREVIOUS: &str = "hestia.previous";
const CANDIDATE: &str = "hestia.new";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Leaves the HTTP answer time to reach the browser before stopping.
const RESTART_DELAY: Duration = Duration::from_millis(800);

#[cfg(target_arch = "aarch64")]
const ELF_MACHINE: u16 = 0xB7;
#[cfg(target_arch = "x86_64")]
const ELF_MACHINE: u16 = 0x3E;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
const ELF_MACHINE: u16 = 0;

fn releases(state: &AppState) -> PathBuf {
    state.setup.config.data_dir.join("releases")
}

pub(crate) async fn status(State(state): State<AppState>, _user: CurrentUser) -> Json<Json_> {
    let releases = releases(&state);
    let resolved = std::fs::canonicalize(&releases).unwrap_or_else(|_| releases.clone());
    let from_update = std::env::current_exe()
        .ok()
        .and_then(|program| std::fs::canonicalize(program).ok())
        .is_some_and(|program| program.starts_with(&resolved));
    Json(json!({
        "version": state.setup.config.version,
        "from_update": from_update,
        "can_roll_back": releases.join(PREVIOUS).exists() || releases.join(CURRENT).exists(),
        "supported": cfg!(target_os = "linux"),
    }))
}

pub(crate) async fn install(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    program: Bytes,
) -> Result<Json<Json_>, ApiError> {
    let signature = headers
        .get("x-hestia-signature")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| release::from_hex(value.trim()))
        .ok_or_else(|| {
            ApiError::Invalid("signature manquante : joins le fichier hestia.sig".to_owned())
        })?;
    let checked = program.clone();
    let signed = tokio::task::spawn_blocking(move || release::verify(&checked, &signature))
        .await
        .map_err(|_| ApiError::Internal)?;
    if !signed {
        warn!(user = user.name, "update refused: bad signature");
        return Err(ApiError::Invalid(
            "fichier refusé : il n'est pas signé par ta clé de publication".to_owned(),
        ));
    }
    if !is_program_for_this_machine(&program) {
        return Err(ApiError::Invalid(
            "ce fichier n'est pas un programme pour cette machine".to_owned(),
        ));
    }
    let releases = releases(&state);
    let candidate = releases.join(CANDIDATE);
    write_program(&candidate, &program).await.map_err(|error| {
        warn!(%error, "cannot write the new program");
        ApiError::Internal
    })?;
    let version = match probe(&candidate).await {
        Ok(version) => version,
        Err(reason) => {
            let _ = tokio::fs::remove_file(&candidate).await;
            return Err(ApiError::Invalid(format!(
                "le nouveau programme ne démarre pas : {reason}"
            )));
        }
    };
    let current = releases.join(CURRENT);
    if current.exists() {
        tokio::fs::rename(&current, releases.join(PREVIOUS))
            .await
            .map_err(|_| ApiError::Internal)?;
    }
    tokio::fs::rename(&candidate, &current)
        .await
        .map_err(|_| ApiError::Internal)?;
    info!(user = user.name, version, "update installed, restarting");
    restart_soon(&state);
    Ok(Json(json!({ "version": version })))
}

/// Swaps the current and previous releases; without a previous one, returns to the
/// program installed by `install.sh`.
pub(crate) async fn roll_back(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Json_>, ApiError> {
    let releases = releases(&state);
    let current = releases.join(CURRENT);
    let previous = releases.join(PREVIOUS);
    let swapped = async {
        if previous.exists() {
            let parked = releases.join("hestia.swap");
            if current.exists() {
                tokio::fs::rename(&current, &parked).await?;
            }
            tokio::fs::rename(&previous, &current).await?;
            if parked.exists() {
                tokio::fs::rename(&parked, &previous).await?;
            }
            Ok(true)
        } else if current.exists() {
            tokio::fs::remove_file(&current).await?;
            Ok(true)
        } else {
            Ok::<_, std::io::Error>(false)
        }
    }
    .await
    .map_err(|_| ApiError::Internal)?;
    if !swapped {
        return Err(ApiError::Invalid("aucune version précédente".to_owned()));
    }
    info!(user = user.name, "rolled back, restarting");
    restart_soon(&state);
    Ok(Json(json!({ "restarting": true })))
}

fn is_program_for_this_machine(program: &[u8]) -> bool {
    program.len() > 64
        && program.starts_with(b"\x7fELF")
        && program[4] == 2
        && u16::from_le_bytes([program[18], program[19]]) == ELF_MACHINE
}

async fn write_program(path: &Path, program: &[u8]) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        tokio::fs::create_dir_all(folder).await?;
    }
    tokio::fs::write(path, program).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await?;
    }
    Ok(())
}

/// The new program must at least start and report its version.
async fn probe(program: &Path) -> Result<String, String> {
    let output = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::process::Command::new(program)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "pas de réponse en 10 s".to_owned())?
    .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && text.starts_with("hestia ") {
        Ok(text.trim_start_matches("hestia ").to_owned())
    } else {
        Err(format!("réponse inattendue « {text} »"))
    }
}

fn restart_soon(state: &AppState) {
    if let Some(restart) = state.setup.config.restart.clone() {
        tokio::spawn(async move {
            tokio::time::sleep(RESTART_DELAY).await;
            restart.notify_one();
        });
    }
}
