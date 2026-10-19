use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::point::PointSpec;

/// Stable identity `<driver instance>:<native id>`, never derived from a name or an address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("device id {0:?} must look like <instance>:<native id>")]
pub struct DeviceIdError(String);

impl DeviceId {
    /// # Errors
    /// Fails when either part is empty or contains whitespace, or when `instance` contains `:`.
    pub fn new(instance: &str, native: &str) -> Result<Self, DeviceIdError> {
        if instance.contains(':') {
            return Err(DeviceIdError(format!("{instance}:{native}")));
        }
        Self::try_from(format!("{instance}:{native}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn instance(&self) -> &str {
        self.0.split_once(':').map_or("", |(instance, _)| instance)
    }

    #[must_use]
    pub fn native(&self) -> &str {
        self.0.split_once(':').map_or("", |(_, native)| native)
    }
}

impl TryFrom<String> for DeviceId {
    type Error = DeviceIdError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        let valid = raw
            .split_once(':')
            .is_some_and(|(instance, native)| !instance.is_empty() && !native.is_empty())
            && !raw.contains(char::is_whitespace);
        if valid {
            Ok(Self(raw))
        } else {
            Err(DeviceIdError(raw))
        }
    }
}

impl From<DeviceId> for String {
    fn from(id: DeviceId) -> Self {
        id.0
    }
}

impl FromStr for DeviceId {
    type Err = DeviceIdError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::try_from(raw.to_owned())
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a driver declares about a device. Display labels (name, room) live in the config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub points: Vec<PointSpec>,
}

impl DeviceInfo {
    #[must_use]
    pub fn point(&self, key: &str) -> Option<&PointSpec> {
        self.points.iter().find(|point| point.key == key)
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceId;

    #[test]
    fn parts_are_split_on_first_colon() {
        let id: DeviceId = "govee:a1b2c3d4e5f60718".parse().unwrap();
        assert_eq!(id.instance(), "govee");
        assert_eq!(id.native(), "a1b2c3d4e5f60718");
    }

    #[test]
    fn malformed_ids_are_rejected() {
        for raw in ["", "govee", ":abc", "govee:", "go vee:abc"] {
            assert!(
                raw.parse::<DeviceId>().is_err(),
                "{raw:?} should be rejected"
            );
        }
        assert!(DeviceId::new("a:b", "c").is_err());
    }
}
