//! Releases published on GitHub. Fetched with the system `curl` so that the program itself
//! carries no TLS code; what is downloaded is trusted only after its signature is checked.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

const MAX_PROGRAM: &str = "67108864";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub published: String,
    pub program_url: String,
    pub signature_url: String,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
}

/// Name of the published program for this machine, e.g. `hestia-aarch64`.
#[must_use]
pub fn asset_name() -> String {
    format!("hestia-{}", std::env::consts::ARCH)
}

/// Tags look like `v0.1.0-20261019.2050` for version `0.1.0+20261019.2050`.
#[must_use]
pub fn tag_for(version: &str) -> String {
    format!("v{}", version.replacen('+', "-", 1))
}

fn version_of(tag: &str) -> String {
    tag.trim_start_matches('v').replacen('-', "+", 1)
}

pub(crate) fn parse_release(json: &[u8]) -> Result<Release, String> {
    let raw: RawRelease =
        serde_json::from_slice(json).map_err(|_| "réponse de GitHub illisible".to_owned())?;
    let program = asset_name();
    let signature = format!("{program}.sig");
    let url = |name: &str| {
        raw.assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
            .ok_or_else(|| format!("la version {} ne contient pas {name}", raw.tag_name))
    };
    Ok(Release {
        version: version_of(&raw.tag_name),
        published: raw.published_at.clone(),
        program_url: url(&program)?,
        signature_url: url(&signature)?,
    })
}

/// `true` when `candidate` is strictly more recent than `current`.
#[must_use]
pub fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

fn parse_version(version: &str) -> (u64, u64, u64, String) {
    let (base, build) = version.split_once('+').unwrap_or((version, ""));
    let mut numbers = base.split('.').map(|part| part.parse().unwrap_or(0));
    (
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        build.to_owned(),
    )
}

/// The latest release of `owner/repo`.
pub(crate) async fn latest(repo: &str) -> Result<Release, String> {
    let valid = repo.split_once('/').is_some_and(|(owner, name)| {
        [owner, name].iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    });
    if !valid {
        return Err(format!("dépôt GitHub invalide : {repo}"));
    }
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let json = curl(
        &[
            "-fsSL",
            "--max-time",
            "20",
            "-H",
            "Accept: application/vnd.github+json",
            &url,
        ],
        Duration::from_secs(30),
    )
    .await?;
    parse_release(&json)
}

/// Downloads a release file; only GitHub addresses are accepted.
pub(crate) async fn download(url: &str, destination: &Path) -> Result<(), String> {
    if !url.starts_with("https://github.com/") {
        return Err(format!("adresse de téléchargement refusée : {url}"));
    }
    let target = destination.to_string_lossy().into_owned();
    curl(
        &[
            "-fsSL",
            "--max-time",
            "300",
            "--max-filesize",
            MAX_PROGRAM,
            "-o",
            &target,
            url,
        ],
        Duration::from_secs(320),
    )
    .await
    .map(|_| ())
}

async fn curl(args: &[&str], limit: Duration) -> Result<Vec<u8>, String> {
    let run = tokio::process::Command::new("curl")
        .args(args)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(limit, run)
        .await
        .map_err(|_| "GitHub ne répond pas".to_owned())?
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => "curl est introuvable sur cette machine".to_owned(),
            _ => error.to_string(),
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if reason.is_empty() {
            "téléchargement impossible".to_owned()
        } else {
            reason
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{asset_name, is_newer, parse_release, tag_for};

    #[test]
    fn versions_compare_by_number_then_build() {
        assert!(is_newer("0.1.0+20261020.0900", "0.1.0+20261019.2050"));
        assert!(is_newer("0.2.0+20261001.0000", "0.1.9+20261231.2359"));
        assert!(
            is_newer("0.1.0+20261019.2050", "0.1.0"),
            "a dev build is older than any release"
        );
        assert!(!is_newer("0.1.0+20261019.2050", "0.1.0+20261019.2050"));
        assert_eq!(tag_for("0.1.0+20261019.2050"), "v0.1.0-20261019.2050");
    }

    #[test]
    fn release_assets_are_found_for_this_machine() {
        let program = asset_name();
        let json = serde_json::json!({
            "tag_name": "v0.1.0-20261019.2050",
            "published_at": "2026-10-19T19:00:00Z",
            "assets": [
                {"name": program, "browser_download_url": "https://github.com/o/r/releases/download/t/p"},
                {"name": format!("{program}.sig"), "browser_download_url": "https://github.com/o/r/releases/download/t/s"}
            ]
        });
        let release = parse_release(json.to_string().as_bytes()).unwrap();
        assert_eq!(release.version, "0.1.0+20261019.2050");
        assert!(release.signature_url.ends_with("/s"));
        let empty = serde_json::json!({"tag_name": "v1.0.0", "assets": []});
        assert!(parse_release(empty.to_string().as_bytes()).is_err());
    }
}
