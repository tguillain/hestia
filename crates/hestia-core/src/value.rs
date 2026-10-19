use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Number(f64),
    Color(Rgb),
    Text(String),
}

impl Value {
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Number(_) => "number",
            Self::Color(_) => "color",
            Self::Text(_) => "text",
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_color(&self) -> Option<Rgb> {
        match self {
            Self::Color(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValueError {
    #[error("expected a {expected} value, got {got}")]
    WrongType {
        expected: &'static str,
        got: &'static str,
    },
    #[error("value {value} is outside [{min}, {max}]")]
    OutOfRange { value: f64, min: f64, max: f64 },
    #[error("value is not a finite number")]
    NotFinite,
    #[error("unknown choice {0:?}")]
    UnknownChoice(String),
    #[error("text longer than {max} characters")]
    TooLong { max: usize },
}

#[cfg(test)]
mod tests {
    use super::{Rgb, Value};

    #[test]
    fn json_round_trip_keeps_each_type() {
        let values = [
            Value::Bool(true),
            Value::Number(42.5),
            Value::Color(Rgb { r: 1, g: 2, b: 3 }),
            Value::Text("blanc".to_owned()),
        ];
        for value in values {
            let json = serde_json::to_string(&value).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), value);
        }
    }

    #[test]
    fn integers_are_numbers() {
        assert_eq!(
            serde_json::from_str::<Value>("40").unwrap(),
            Value::Number(40.0)
        );
    }
}
