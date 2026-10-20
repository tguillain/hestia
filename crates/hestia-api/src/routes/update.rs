//! Updating the program from the Settings page. Releases live in `<data>/releases/`:
//! `hestia` is started instead of the installed program when present, `hestia.previous`
//! is kept for rolling back.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use serde_json::{Value as Json_, json};
use tracing::{info, warn};

use super::{CurrentUser, SetupState};
use crate::AppState;
use crate::error::ApiError;
use crate::{github, release};

const CURRENT: &str = "hestia";
const PREVIOUS: &str = "hestia.previous";
const CANDIDATE: &str = "hestia.new";
const DOWNLOAD: &str = "hestia.download";
/// Present while a fresh release has not proved it starts: `launch` counts its starts
/// and brings the previous release back after a few failures.
const TRIAL: &str = "trial";
/// Written by `launch` after such a rollback: `<unix seconds> <version>`.
const ROLLED_BACK: &str = "rolled-back";
/// Running this long counts as a good start.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Leaves the HTTP answer time to reach the browser before stopping.
const RESTART_DELAY: Duration = Duration::from_millis(800);
const WATCH_FIRST: Duration = Duration::from_secs(60);
const WATCH_EVERY: Duration = Duration::from_secs(24 * 3600);
const WATCH_RETRY: Duration = Duration::from_secs(3600);

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
    let rolled_back = std::fs::read_to_string(releases.join(ROLLED_BACK))
        .ok()
        .and_then(|record| {
            let (at, version) = record.trim().split_once(' ').unwrap_or((record.trim(), ""));
            Some(json!({ "at": at.parse::<i64>().ok()?, "version": version }))
        });
    Json(json!({
        "version": state.setup.config.version,
        "from_update": from_update,
        "can_roll_back": releases.join(PREVIOUS).exists() || releases.join(CURRENT).exists(),
        "supported": cfg!(target_os = "linux"),
        "rolled_back": rolled_back,
    }))
}

/// The running release has been up for [`HEALTHY_AFTER`]: it is no longer on trial.
pub fn confirm_release(data_dir: &Path) {
    let trial = data_dir.join("releases").join(TRIAL);
    if trial.exists() {
        match std::fs::remove_file(&trial) {
            Ok(()) => info!("new release confirmed"),
            Err(error) => warn!(%error, "cannot confirm the new release"),
        }
    }
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
    let version = install_verified(&state, &user.name, program.to_vec(), signature).await?;
    Ok(Json(json!({ "version": version })))
}

/// What GitHub offers compared with the running version.
pub(crate) async fn latest_report(state: &AppState) -> Result<Json_, ApiError> {
    let latest = github::latest(update_repo(state)?)
        .await
        .map_err(ApiError::BadGateway)?;
    let current = &state.setup.config.version;
    let available = github::is_newer(&latest.version, current);
    state
        .setup
        .set_available_update(available.then(|| latest.version.clone()));
    Ok(json!({
        "current": current,
        "latest": latest.version,
        "published": latest.published,
        "update_available": available,
    }))
}

/// Looks for a newer release a minute after start-up, then once a day, so the dashboard
/// can show that one is waiting.
pub(crate) async fn watch(setup: Arc<SetupState>) {
    let Some(repo) = setup.config.update_repo.clone() else {
        return;
    };
    tokio::time::sleep(WATCH_FIRST).await;
    loop {
        let wait = match github::latest(&repo).await {
            Ok(latest) => {
                let available = github::is_newer(&latest.version, &setup.config.version);
                if available {
                    info!(version = latest.version, "update available");
                }
                setup.set_available_update(available.then_some(latest.version));
                WATCH_EVERY
            }
            Err(reason) => {
                warn!(reason, "cannot check for updates");
                WATCH_RETRY
            }
        };
        tokio::time::sleep(wait).await;
    }
}

/// Downloads the latest signed release from GitHub and installs it.
pub(crate) async fn install_latest(state: &AppState, who: &str) -> Result<String, ApiError> {
    let latest = github::latest(update_repo(state)?)
        .await
        .map_err(ApiError::BadGateway)?;
    let current = &state.setup.config.version;
    if !github::is_newer(&latest.version, current) {
        return Err(ApiError::Invalid(format!(
            "Hestia est déjà à jour (version {current})"
        )));
    }
    let releases = releases(state);
    tokio::fs::create_dir_all(&releases)
        .await
        .map_err(|_| ApiError::Internal)?;
    let program_path = releases.join(DOWNLOAD);
    let signature_path = releases.join(format!("{DOWNLOAD}.sig"));
    let downloaded = async {
        github::download(&latest.program_url, &program_path).await?;
        github::download(&latest.signature_url, &signature_path).await?;
        let program = tokio::fs::read(&program_path)
            .await
            .map_err(|error| error.to_string())?;
        let signature = tokio::fs::read_to_string(&signature_path)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((program, signature))
    }
    .await;
    let _ = tokio::fs::remove_file(&program_path).await;
    let _ = tokio::fs::remove_file(&signature_path).await;
    let (program, signature) = downloaded.map_err(ApiError::BadGateway)?;
    let signature = release::from_hex(signature.trim())
        .ok_or_else(|| ApiError::Invalid("signature publiée illisible".to_owned()))?;
    info!(
        who,
        version = latest.version,
        "installing release from GitHub"
    );
    install_verified(state, who, program, signature).await
}

pub(crate) async fn latest(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<Json_>, ApiError> {
    latest_report(&state).await.map(Json)
}

pub(crate) async fn install_from_github(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Json_>, ApiError> {
    let version = install_latest(&state, &user.name).await?;
    Ok(Json(json!({ "version": version })))
}

fn update_repo(state: &AppState) -> Result<&str, ApiError> {
    state
        .setup
        .config
        .update_repo
        .as_deref()
        .ok_or_else(|| ApiError::NotFound("aucun dépôt de mises à jour configuré".to_owned()))
}

/// Signature, machine and start-up checks, then the switch and the restart.
async fn install_verified(
    state: &AppState,
    who: &str,
    program: Vec<u8>,
    signature: Vec<u8>,
) -> Result<String, ApiError> {
    let (program, signed) = tokio::task::spawn_blocking(move || {
        let signed = release::verify(&program, &signature);
        (program, signed)
    })
    .await
    .map_err(|_| ApiError::Internal)?;
    if !signed {
        warn!(who, "update refused: bad signature");
        return Err(ApiError::Invalid(
            "fichier refusé : il n'est pas signé par ta clé de publication".to_owned(),
        ));
    }
    if !is_program_for_this_machine(&program) {
        return Err(ApiError::Invalid(
            "ce fichier n'est pas un programme pour cette machine".to_owned(),
        ));
    }
    let releases = releases(state);
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
    if let Err(error) = tokio::fs::write(releases.join(TRIAL), "0\n").await {
        warn!(%error, "cannot put the new release on trial");
    }
    let _ = tokio::fs::remove_file(releases.join(ROLLED_BACK)).await;
    info!(who, version, "update installed, restarting");
    restart_soon(state);
    Ok(version)
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
    // The previous release already proved itself.
    let _ = tokio::fs::remove_file(releases.join(TRIAL)).await;
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
