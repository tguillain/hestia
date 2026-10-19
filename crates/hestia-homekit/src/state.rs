use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hestia_core::DeviceId;
use hestia_hub::storage::{load_json, save_json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

use crate::crypto::random;

pub const MAX_PAIRINGS: usize = 16;
const SETUP_ID_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// Codes Apple refuses because they are trivial to guess.
const FORBIDDEN_CODES: [u32; 12] = [
    0, 11_111_111, 22_222_222, 33_333_333, 44_444_444, 55_555_555, 66_666_666, 77_777_777,
    88_888_888, 99_999_999, 12_345_678, 87_654_321,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    pub id: String,
    pub public_key: String,
    pub admin: bool,
}

impl Pairing {
    pub fn verifying_key(&self) -> Option<VerifyingKey> {
        let bytes: [u8; 32] = from_hex(&self.public_key)?.try_into().ok()?;
        VerifyingKey::from_bytes(&bytes).ok()
    }
}

/// What the bridge needs to expose a light, remembered so the accessory survives restarts
/// and outages: a light missing from the bridge would be deleted from the Home app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightShape {
    pub name: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub on: String,
    pub brightness: String,
    pub color: Option<String>,
    pub temperature: Option<Temperature>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Temperature {
    pub key: String,
    pub min_kelvin: f64,
    pub max_kelvin: f64,
}

impl LightShape {
    fn same_services(&self, other: &Self) -> bool {
        self.color.is_some() == other.color.is_some()
            && self.temperature.is_some() == other.temperature.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredLight {
    pub aid: u64,
    #[serde(flatten)]
    pub shape: LightShape,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    device_id: String,
    signing_seed: String,
    setup_code: String,
    setup_id: String,
    config_number: u32,
    next_aid: u64,
    lights: BTreeMap<DeviceId, StoredLight>,
    pairings: Vec<Pairing>,
}

/// Everything `HomeKit` expects to survive a restart: identity, setup code, paired controllers
/// and stable accessory ids. Lose it and every iPhone has to pair again.
pub struct BridgeState {
    path: PathBuf,
    stored: Stored,
    signing: SigningKey,
}

impl BridgeState {
    /// # Errors
    /// Fails when the file is unreadable or cannot be created.
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join("homekit.json");
        let mut stored: Stored = load_json(&path)?;
        let mut changed = false;
        if stored.device_id.is_empty() {
            stored.device_id = random::<6>()
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(":");
            changed = true;
        }
        if from_hex(&stored.signing_seed).is_none_or(|seed| seed.len() != 32) {
            stored.signing_seed = to_hex(&random::<32>());
            changed = true;
        }
        if stored.setup_code.is_empty() {
            stored.setup_code = new_setup_code();
            changed = true;
        }
        if stored.setup_id.len() != 4 {
            stored.setup_id = random::<4>()
                .iter()
                .map(|byte| {
                    char::from(SETUP_ID_ALPHABET[usize::from(*byte) % SETUP_ID_ALPHABET.len()])
                })
                .collect();
            changed = true;
        }
        if stored.config_number == 0 {
            stored.config_number = 1;
            changed = true;
        }
        let seed: [u8; 32] = from_hex(&stored.signing_seed)
            .and_then(|seed| seed.try_into().ok())
            .context("invalid signing seed")?;
        let state = Self {
            path,
            stored,
            signing: SigningKey::from_bytes(&seed),
        };
        if changed {
            state.save()?;
        }
        Ok(state)
    }

    /// The accessory pairing id, shaped like a MAC address.
    pub fn device_id(&self) -> &str {
        &self.stored.device_id
    }

    pub fn setup_code(&self) -> &str {
        &self.stored.setup_code
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    pub fn config_number(&self) -> u32 {
        self.stored.config_number
    }

    pub fn is_paired(&self) -> bool {
        !self.stored.pairings.is_empty()
    }

    pub fn pairings(&self) -> &[Pairing] {
        &self.stored.pairings
    }

    pub fn pairing(&self, id: &str) -> Option<&Pairing> {
        self.stored.pairings.iter().find(|pairing| pairing.id == id)
    }

    /// # Errors
    /// Fails when the identifier is already paired with another key, when the
    /// table is full, or when the file cannot be written.
    pub fn add_pairing(
        &mut self,
        id: &str,
        public_key: &[u8; 32],
        admin: bool,
    ) -> anyhow::Result<()> {
        let key = to_hex(public_key);
        if let Some(existing) = self
            .stored
            .pairings
            .iter_mut()
            .find(|pairing| pairing.id == id)
        {
            if existing.public_key != key {
                bail!("controller {id} is already paired with another key");
            }
            existing.admin = admin;
        } else {
            if self.stored.pairings.len() >= MAX_PAIRINGS {
                bail!("too many pairings");
            }
            self.stored.pairings.push(Pairing {
                id: id.to_owned(),
                public_key: key,
                admin,
            });
        }
        self.save()
    }

    /// Removing the last admin unpairs the bridge entirely, as the specification requires.
    ///
    /// # Errors
    /// Fails when the file cannot be written.
    pub fn remove_pairing(&mut self, id: &str) -> anyhow::Result<()> {
        self.stored.pairings.retain(|pairing| pairing.id != id);
        if !self.stored.pairings.iter().any(|pairing| pairing.admin) {
            self.stored.pairings.clear();
        }
        self.save()
    }

    pub fn lights(&self) -> &BTreeMap<DeviceId, StoredLight> {
        &self.stored.lights
    }

    /// Records a light. New lights get a fresh accessory id (never reused); any change in
    /// the services bumps the configuration number so controllers reload the list.
    ///
    /// # Errors
    /// Fails when the file cannot be written.
    pub fn remember_light(&mut self, device: &DeviceId, shape: LightShape) -> anyhow::Result<bool> {
        let layout_changed = match self.stored.lights.get_mut(device) {
            Some(existing) if existing.shape == shape => return Ok(false),
            Some(existing) => {
                let changed = !existing.shape.same_services(&shape);
                existing.shape = shape;
                changed
            }
            None => {
                let aid = self.stored.next_aid.max(2);
                self.stored.next_aid = aid + 1;
                self.stored
                    .lights
                    .insert(device.clone(), StoredLight { aid, shape });
                true
            }
        };
        if layout_changed {
            self.stored.config_number = self.stored.config_number.checked_add(1).unwrap_or(1);
        }
        self.save()?;
        Ok(layout_changed)
    }

    /// # Errors
    /// Fails when the file cannot be written.
    pub fn forget_light(&mut self, device: &DeviceId) -> anyhow::Result<bool> {
        if self.stored.lights.remove(device).is_none() {
            return Ok(false);
        }
        self.stored.config_number = self.stored.config_number.checked_add(1).unwrap_or(1);
        self.save()?;
        Ok(true)
    }

    /// Value of the `sh` TXT record, which lets iOS match a scanned QR code.
    pub fn setup_hash(&self) -> String {
        let digest = Sha512::digest(format!("{}{}", self.stored.setup_id, self.stored.device_id));
        base64(&digest[..4])
    }

    /// `X-HM://` payload encoded in the pairing QR code.
    pub fn setup_uri(&self) -> String {
        const CATEGORY_BRIDGE: u64 = 2;
        const FLAG_IP: u64 = 2;
        let code: u64 = self
            .stored
            .setup_code
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        let payload = (CATEGORY_BRIDGE << 31) | (FLAG_IP << 27) | (code & 0x7FF_FFFF);
        format!("X-HM://{:0>9}{}", base36(payload), self.stored.setup_id)
    }

    fn save(&self) -> anyhow::Result<()> {
        save_json(&self.path, &self.stored)
    }
}

fn new_setup_code() -> String {
    loop {
        let bytes = random::<4>();
        let number = u32::from_le_bytes(bytes) % 100_000_000;
        if !FORBIDDEN_CODES.contains(&number) {
            let digits = format!("{number:08}");
            return format!("{}-{}-{}", &digits[..3], &digits[3..5], &digits[5..]);
        }
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[usize::try_from(value % 36).unwrap_or(0)]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let buffer = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let group =
            (u32::from(buffer[0]) << 16) | (u32::from(buffer[1]) << 8) | u32::from(buffer[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (group >> (18 - 6 * index)) & 0x3F;
                out.push(char::from(ALPHABET[usize::try_from(sextet).unwrap_or(0)]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{BridgeState, LightShape, base36, base64, from_hex, to_hex};

    #[test]
    fn identity_is_created_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("hestia-homekit-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = BridgeState::open(&dir).unwrap();
        let code = first.setup_code().to_owned();
        assert_eq!(code.len(), 10);
        assert_eq!(first.device_id().len(), 17);
        let second = BridgeState::open(&dir).unwrap();
        assert_eq!(second.setup_code(), code);
        assert_eq!(
            second.signing_key().to_bytes(),
            first.signing_key().to_bytes()
        );
    }

    #[test]
    fn pairings_and_aids() {
        let dir =
            std::env::temp_dir().join(format!("hestia-homekit-pairings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut state = BridgeState::open(&dir).unwrap();
        state.add_pairing("admin", &[1; 32], true).unwrap();
        state.add_pairing("guest", &[2; 32], false).unwrap();
        assert!(state.add_pairing("guest", &[3; 32], false).is_err());
        assert!(state.is_paired());
        state.remove_pairing("admin").unwrap();
        assert!(
            !state.is_paired(),
            "removing the last admin unpairs everything"
        );
    }

    #[test]
    fn lights_keep_their_ids_and_bump_the_configuration() {
        let dir =
            std::env::temp_dir().join(format!("hestia-homekit-lights-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut state = BridgeState::open(&dir).unwrap();
        let shape = |name: &str, color: bool| LightShape {
            name: name.to_owned(),
            manufacturer: None,
            model: None,
            on: "on".to_owned(),
            brightness: "brightness".to_owned(),
            color: color.then(|| "color".to_owned()),
            temperature: None,
        };
        let lamp = "govee:a".parse().unwrap();
        let other = "govee:b".parse().unwrap();
        let start = state.config_number();
        assert!(
            state
                .remember_light(&lamp, shape("Chambre", false))
                .unwrap()
        );
        assert!(
            state
                .remember_light(&other, shape("Cuisine", false))
                .unwrap()
        );
        assert!(
            !state
                .remember_light(&lamp, shape("Chambre", false))
                .unwrap()
        );
        assert!(
            !state
                .remember_light(&lamp, shape("Chambre 2", false))
                .unwrap(),
            "a rename keeps the layout"
        );
        assert!(
            state
                .remember_light(&lamp, shape("Chambre 2", true))
                .unwrap()
        );
        assert_eq!(state.config_number(), start + 3);
        assert_eq!(state.lights()[&lamp].aid, 2);
        assert_eq!(state.lights()[&other].aid, 3);

        let reopened = BridgeState::open(&dir).unwrap();
        assert_eq!(reopened.lights()[&other].aid, 3);
    }

    #[test]
    fn encodings() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(36 * 36 + 35), "10Z");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(&[0xde, 0xad, 0xbe, 0xef]), "3q2+7w==");
        assert_eq!(from_hex(&to_hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
    }
}
