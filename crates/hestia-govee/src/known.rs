//! Lamp addresses kept in `<data>/govee.json`: lamps found before, polled directly even
//! when their multicast replies get lost, and addresses added by hand from the dashboard.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use hestia_hub::storage::{load_json, save_json};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownLamps {
    /// Addresses typed in the Settings page.
    #[serde(default)]
    pub manual: Vec<Ipv4Addr>,
    /// Last address of each lamp seen, by native id.
    #[serde(default)]
    pub seen: BTreeMap<String, Ipv4Addr>,
}

impl KnownLamps {
    /// Every address worth a direct (unicast) scan.
    #[must_use]
    pub fn addresses(&self) -> Vec<Ipv4Addr> {
        let mut addresses: Vec<Ipv4Addr> = self
            .manual
            .iter()
            .chain(self.seen.values())
            .copied()
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        addresses
    }
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("govee.json")
}

/// A missing or unreadable file reads as empty: the driver must keep working.
#[must_use]
pub fn load_known(data_dir: &Path) -> KnownLamps {
    load_json(&path(data_dir)).unwrap_or_default()
}

/// # Errors
/// Fails when the file cannot be written.
pub fn save_known(data_dir: &Path, known: &KnownLamps) -> anyhow::Result<()> {
    save_json(&path(data_dir), known)
}

/// # Errors
/// Fails when the file cannot be written.
pub fn add_manual_address(data_dir: &Path, address: Ipv4Addr) -> anyhow::Result<bool> {
    let mut known = load_known(data_dir);
    if known.manual.contains(&address) {
        return Ok(false);
    }
    known.manual.push(address);
    save_known(data_dir, &known)?;
    Ok(true)
}

/// # Errors
/// Fails when the file cannot be written.
pub fn remove_manual_address(data_dir: &Path, address: Ipv4Addr) -> anyhow::Result<bool> {
    let mut known = load_known(data_dir);
    let before = known.manual.len();
    known.manual.retain(|candidate| *candidate != address);
    if known.manual.len() == before {
        return Ok(false);
    }
    save_known(data_dir, &known)?;
    Ok(true)
}

/// The /24 around `address`, without the network, broadcast and `address` itself.
#[must_use]
pub fn neighbours(address: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, own] = address.octets();
    (1..=254)
        .filter(|host| *host != own)
        .map(|host| Ipv4Addr::new(a, b, c, host))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{KnownLamps, add_manual_address, load_known, neighbours, remove_manual_address};
    use std::net::Ipv4Addr;

    #[test]
    fn manual_addresses_round_trip() {
        let dir = std::env::temp_dir().join(format!("hestia-govee-known-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let address = Ipv4Addr::new(192, 168, 1, 40);
        assert!(add_manual_address(&dir, address).unwrap());
        assert!(!add_manual_address(&dir, address).unwrap());
        assert_eq!(load_known(&dir).manual, [address]);
        assert!(remove_manual_address(&dir, address).unwrap());
        assert_eq!(load_known(&dir).manual, [] as [std::net::Ipv4Addr; 0]);
    }

    #[test]
    fn addresses_are_deduplicated_and_neighbours_skip_ourselves() {
        let mut known = KnownLamps::default();
        known.manual.push(Ipv4Addr::new(192, 168, 1, 31));
        known
            .seen
            .insert("a".to_owned(), Ipv4Addr::new(192, 168, 1, 31));
        assert_eq!(known.addresses().len(), 1);
        let around = neighbours(Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(around.len(), 253);
        assert!(!around.contains(&Ipv4Addr::new(192, 168, 1, 20)));
    }
}
