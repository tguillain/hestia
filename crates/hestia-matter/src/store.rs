//! Hestia's Matter fabric and the devices on it, kept in `<data>/matter.json`.
//!
//! The file holds the fabric's root key: whoever reads it can control every paired device.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use hestia_hub::storage::{load_json, save_json};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Controller {
    /// Created on first start.
    #[serde(default)]
    pub fabric: Option<FabricKeys>,
    #[serde(default)]
    pub devices: Vec<PairedDevice>,
}

/// Everything needed to rebuild the fabric at start-up. Certificates are Matter TLV.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FabricKeys {
    pub fabric_id: u64,
    /// Hestia's own node id on the fabric.
    pub node_id: u64,
    pub root_certificate: Hex,
    /// Signs the certificate of each new device.
    pub root_key: Hex,
    pub certificate: Hex,
    pub key: Hex,
    /// Identity protection key shared with every device.
    pub ipk: Hex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub node_id: u64,
    pub vendor: String,
    pub product: String,
    /// Endpoint carrying the light clusters.
    pub endpoint: u16,
    pub features: Features,
    /// Last address the device answered from, tried before asking mDNS.
    #[serde(default)]
    pub address: Option<SocketAddr>,
}

/// What a light can do, read once at pairing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Features {
    pub level: bool,
    pub hue_saturation: bool,
    pub xy: bool,
    /// Colour temperature range in mireds, when the light has tunable white.
    pub mireds: Option<(u16, u16)>,
}

impl PairedDevice {
    /// The id part after `matter:`.
    #[must_use]
    pub fn native_id(&self) -> String {
        native_id(self.node_id)
    }
}

#[must_use]
pub fn native_id(node_id: u64) -> String {
    format!("{node_id:016x}")
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("matter.json")
}

/// # Errors
/// Fails when the file exists but cannot be read: starting a new fabric would orphan
/// every paired device.
pub fn load_controller(data_dir: &Path) -> anyhow::Result<Controller> {
    load_json(&path(data_dir))
}

/// # Errors
/// Fails when the file cannot be written.
pub fn save_controller(data_dir: &Path, controller: &Controller) -> anyhow::Result<()> {
    save_json(&path(data_dir), controller)
}

/// Bytes written as a hexadecimal string.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Hex(pub Vec<u8>);

impl std::fmt::Debug for Hex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Hex({} bytes)", self.0.len())
    }
}

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write as _;
        let mut text = String::with_capacity(self.0.len() * 2);
        for byte in &self.0 {
            let _ = write!(text, "{byte:02x}");
        }
        serializer.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() % 2 != 0 {
            return Err(serde::de::Error::custom("odd number of hexadecimal digits"));
        }
        (0..text.len())
            .step_by(2)
            .map(|start| u8::from_str_radix(&text[start..start + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map(Hex)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{Controller, Features, Hex, PairedDevice, load_controller, save_controller};

    #[test]
    fn controller_round_trip() {
        let dir = std::env::temp_dir().join(format!("hestia-matter-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_controller(&dir).unwrap(), Controller::default());
        let controller = Controller {
            fabric: None,
            devices: vec![PairedDevice {
                node_id: 0x00ab,
                vendor: "Govee".to_owned(),
                product: "H6047".to_owned(),
                endpoint: 1,
                features: Features {
                    level: true,
                    hue_saturation: true,
                    xy: true,
                    mireds: Some((153, 370)),
                },
                address: Some("[fe80::1]:5540".parse().unwrap()),
            }],
        };
        save_controller(&dir, &controller).unwrap();
        assert_eq!(load_controller(&dir).unwrap(), controller);
        assert_eq!(controller.devices[0].native_id(), "00000000000000ab");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_is_lowercase_and_strict() {
        let json = serde_json::to_string(&Hex(vec![0x0a, 0xff])).unwrap();
        assert_eq!(json, "\"0aff\"");
        assert_eq!(
            serde_json::from_str::<Hex>(&json).unwrap(),
            Hex(vec![0x0a, 0xff])
        );
        assert!(serde_json::from_str::<Hex>("\"abc\"").is_err());
        assert!(serde_json::from_str::<Hex>("\"zz\"").is_err());
    }
}
