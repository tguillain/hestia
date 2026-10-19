use serde::{Deserialize, Serialize};

use crate::value::{Value, ValueError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PointKind {
    Binary,
    Numeric { min: f64, max: f64, step: f64 },
    Enum { values: Vec<String> },
    Text { max_len: usize },
    Color,
}

impl PointKind {
    #[must_use]
    pub fn value_type(&self) -> &'static str {
        match self {
            Self::Binary => "bool",
            Self::Numeric { .. } => "number",
            Self::Enum { .. } | Self::Text { .. } => "text",
            Self::Color => "color",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Percent,
    Kelvin,
    Celsius,
    Millisecond,
    BitPerSecond,
    RevolutionPerMinute,
    Byte,
}

impl Unit {
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Percent => "%",
            Self::Kelvin => "K",
            Self::Celsius => "°C",
            Self::Millisecond => "ms",
            Self::BitPerSecond => "bit/s",
            Self::RevolutionPerMinute => "tr/min",
            Self::Byte => "o",
        }
    }
}

/// Brand-agnostic meaning of a point, used by the UI and the `HomeKit` bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Semantic {
    OnOff,
    Brightness,
    Color,
    ColorTemperature,
    ColorMode,
    Temperature,
    Connectivity,
    Throughput,
    Latency,
    Count,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointSpec {
    pub key: String,
    pub label: String,
    pub kind: PointKind,
    pub access: Access,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Unit>,
    pub semantic: Semantic,
}

impl PointSpec {
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        kind: PointKind,
        semantic: Semantic,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind,
            access: Access::Read,
            unit: None,
            semantic,
        }
    }

    #[must_use]
    pub fn writable(mut self) -> Self {
        self.access = Access::ReadWrite;
        self
    }

    #[must_use]
    pub fn with_unit(mut self, unit: Unit) -> Self {
        self.unit = Some(unit);
        self
    }

    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.access == Access::ReadWrite
    }

    /// Checks that `value` fits this point's kind and bounds.
    ///
    /// # Errors
    /// Returns the first rule the value breaks.
    pub fn validate(&self, value: &Value) -> Result<(), ValueError> {
        match (&self.kind, value) {
            (PointKind::Binary, Value::Bool(_)) | (PointKind::Color, Value::Color(_)) => Ok(()),
            (PointKind::Numeric { min, max, .. }, Value::Number(number)) => {
                if !number.is_finite() {
                    Err(ValueError::NotFinite)
                } else if number < min || number > max {
                    Err(ValueError::OutOfRange {
                        value: *number,
                        min: *min,
                        max: *max,
                    })
                } else {
                    Ok(())
                }
            }
            (PointKind::Enum { values }, Value::Text(text)) => {
                if values.iter().any(|choice| choice == text) {
                    Ok(())
                } else {
                    Err(ValueError::UnknownChoice(text.clone()))
                }
            }
            (PointKind::Text { max_len }, Value::Text(text)) => {
                if text.chars().count() > *max_len {
                    Err(ValueError::TooLong { max: *max_len })
                } else {
                    Ok(())
                }
            }
            (kind, value) => Err(ValueError::WrongType {
                expected: kind.value_type(),
                got: value.type_name(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PointKind, PointSpec, Semantic};
    use crate::value::{Rgb, Value, ValueError};

    fn brightness() -> PointSpec {
        PointSpec::new(
            "brightness",
            "Luminosité",
            PointKind::Numeric {
                min: 0.0,
                max: 100.0,
                step: 1.0,
            },
            Semantic::Brightness,
        )
        .writable()
    }

    #[test]
    fn numeric_bounds_are_enforced() {
        let spec = brightness();
        assert!(spec.validate(&Value::Number(40.0)).is_ok());
        assert!(matches!(
            spec.validate(&Value::Number(140.0)),
            Err(ValueError::OutOfRange { .. })
        ));
        assert_eq!(
            spec.validate(&Value::Number(f64::NAN)),
            Err(ValueError::NotFinite)
        );
    }

    #[test]
    fn wrong_type_is_rejected() {
        assert_eq!(
            brightness().validate(&Value::Bool(true)),
            Err(ValueError::WrongType {
                expected: "number",
                got: "bool"
            })
        );
        let color = PointSpec::new("color", "Couleur", PointKind::Color, Semantic::Color);
        assert!(
            color
                .validate(&Value::Color(Rgb { r: 255, g: 0, b: 0 }))
                .is_ok()
        );
    }

    #[test]
    fn enum_and_text_rules() {
        let mode = PointSpec::new(
            "mode",
            "Mode",
            PointKind::Enum {
                values: vec!["color".to_owned(), "white".to_owned()],
            },
            Semantic::ColorMode,
        );
        assert!(mode.validate(&Value::Text("white".to_owned())).is_ok());
        assert!(mode.validate(&Value::Text("disco".to_owned())).is_err());

        let text = PointSpec::new(
            "motd",
            "Message",
            PointKind::Text { max_len: 3 },
            Semantic::Other,
        );
        assert_eq!(
            text.validate(&Value::Text("abcd".to_owned())),
            Err(ValueError::TooLong { max: 3 })
        );
    }
}
