//! Who is home, from the phones the Freebox sees on the local network. The people to
//! follow are chosen in the Settings page and kept in `<data>/freebox-presence.json`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use hestia_hub::storage::{load_json, save_json};
use serde::{Deserialize, Serialize};

use crate::api::FreeboxApi;
use crate::driver::FreeboxConfig;
use crate::model::LanHost;
use crate::token::load_token;

/// Freebox answers listing the hosts of the main network interface.
pub(crate) const LAN_HOSTS: &str = "lan/browser/pub/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// Device id suffix: `freebox:person-<id>`.
    pub id: String,
    pub name: String,
    /// MAC address of the phone, lowercase.
    pub mac: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct People {
    #[serde(default)]
    pub people: Vec<Person>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("freebox-presence.json")
}

/// A missing or unreadable file reads as nobody followed: the driver keeps working.
#[must_use]
pub fn load_people(data_dir: &Path) -> People {
    load_json(&path(data_dir)).unwrap_or_default()
}

/// # Errors
/// Fails when the name or the address is unusable, the phone is already followed, or the
/// file cannot be written.
pub fn add_person(data_dir: &Path, name: &str, mac: &str) -> anyhow::Result<Person> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        bail!("prénom de 1 à 40 caractères");
    }
    let mac = normalize_mac(mac).with_context(|| format!("« {mac} » n'est pas une adresse MAC"))?;
    let mut people = load_people(data_dir);
    if people.people.iter().any(|person| person.mac == mac) {
        bail!("ce téléphone est déjà suivi");
    }
    let base = slug(name);
    let mut id = base.clone();
    let mut counter = 2;
    while people.people.iter().any(|person| person.id == id) {
        id = format!("{base}-{counter}");
        counter += 1;
    }
    let person = Person {
        id,
        name: name.to_owned(),
        mac,
    };
    people.people.push(person.clone());
    save_json(&path(data_dir), &people)?;
    Ok(person)
}

/// # Errors
/// Fails when the file cannot be written.
pub fn remove_person(data_dir: &Path, id: &str) -> anyhow::Result<bool> {
    let mut people = load_people(data_dir);
    let before = people.people.len();
    people.people.retain(|person| person.id != id);
    if people.people.len() == before {
        return Ok(false);
    }
    save_json(&path(data_dir), &people)?;
    Ok(true)
}

/// The hosts the Freebox knows, phones first, for choosing whom to follow.
///
/// # Errors
/// Fails when Hestia is not authorised on the box, the box is unreachable, or the
/// application lacks the right to browse the network.
pub async fn lan_hosts(config: &FreeboxConfig, data_dir: &Path) -> anyhow::Result<Vec<LanHost>> {
    let Some(token) = load_token(data_dir)? else {
        bail!("Hestia n'est pas encore autorisé sur la Freebox");
    };
    let (mut api, _) = FreeboxApi::discover(&config.host, config.port).await?;
    api.login(&token.app_token).await?;
    let mut hosts: Vec<LanHost> = api.get(LAN_HOSTS).await?;
    hosts.retain(|host| host.mac().is_some());
    hosts.sort_by(|left, right| {
        (!left.is_phone(), !left.active, left.name().to_lowercase()).cmp(&(
            !right.is_phone(),
            !right.active,
            right.name().to_lowercase(),
        ))
    });
    Ok(hosts)
}

/// Home when the phone is connected, or was seen less than `grace_s` ago: phones put
/// their Wi-Fi to sleep for minutes at a time.
#[must_use]
pub fn is_home(host: Option<&LanHost>, now_s: u64, grace_s: u64) -> bool {
    host.is_some_and(|host| {
        host.active || host.reachable || now_s.saturating_sub(host.last_time_reachable) < grace_s
    })
}

#[must_use]
pub fn normalize_mac(raw: &str) -> Option<String> {
    let hex: String = raw
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.' | ' '))
        .collect();
    if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let lower = hex.to_ascii_lowercase();
    Some(
        (0..6)
            .map(|byte| &lower[byte * 2..byte * 2 + 2])
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// `Hélène Dupont` becomes `helene-dupont`.
fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let plain = match c {
            'à' | 'â' | 'ä' | 'á' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ÿ' => 'y',
            other => other,
        };
        if plain.is_ascii_alphanumeric() {
            slug.push(plain);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "personne".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::{add_person, is_home, load_people, normalize_mac, remove_person, slug};
    use crate::model::LanHost;

    fn host(active: bool, last_time_reachable: u64) -> LanHost {
        serde_json::from_value(serde_json::json!({
            "primary_name": "iPhone",
            "active": active,
            "reachable": false,
            "last_time_reachable": last_time_reachable,
            "l2ident": {"id": "AA:BB:CC:DD:EE:FF", "type": "mac_address"},
        }))
        .unwrap()
    }

    #[test]
    fn grace_keeps_a_sleeping_phone_home() {
        assert!(is_home(Some(&host(true, 0)), 10_000, 600));
        assert!(is_home(Some(&host(false, 9_700)), 10_000, 600));
        assert!(!is_home(Some(&host(false, 9_000)), 10_000, 600));
        assert!(!is_home(None, 10_000, 600));
    }

    #[test]
    fn names_and_addresses() {
        assert_eq!(slug("Hélène Dupont"), "helene-dupont");
        assert_eq!(slug("  ?? "), "personne");
        assert_eq!(
            normalize_mac("AA-BB-CC-DD-EE-0F").as_deref(),
            Some("aa:bb:cc:dd:ee:0f")
        );
        assert_eq!(
            normalize_mac("aabb.ccdd.ee0f").as_deref(),
            Some("aa:bb:cc:dd:ee:0f")
        );
        assert_eq!(normalize_mac("aa:bb:cc"), None);
        assert_eq!(host(true, 0).mac().as_deref(), Some("aa:bb:cc:dd:ee:ff"));
    }

    #[test]
    fn people_round_trip() {
        let dir = std::env::temp_dir().join(format!("hestia-presence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let thomas = add_person(&dir, "Thomas", "AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(thomas.id, "thomas");
        assert!(
            add_person(&dir, "Autre", "aa-bb-cc-dd-ee-ff").is_err(),
            "same phone"
        );
        let other = add_person(&dir, "Thomas", "11:22:33:44:55:66").unwrap();
        assert_eq!(other.id, "thomas-2");
        assert!(remove_person(&dir, "thomas").unwrap());
        assert!(!remove_person(&dir, "thomas").unwrap());
        assert_eq!(load_people(&dir).people, [other]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
