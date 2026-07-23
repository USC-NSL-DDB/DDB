use std::{
    collections::HashMap,
    fmt::Display,
    ops::{Deref, DerefMut, Index, IndexMut},
};

use serde::Serialize;

/// Backend-neutral structured value produced by debugger protocols.
///
/// Debuggers tend to expose scalar values as rendered strings even when the
/// underlying value is numeric. Keeping that representation here preserves
/// DDB's established wire schema without leaking GDB/MI types into command
/// flow or framework plugins.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub enum Value {
    String(String),
    List(List),
    Dict(Dict),
}

pub type List = Vec<Value>;

/// Backend-neutral debugger record payload.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct Dict(pub HashMap<String, Value>);

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum ValueError {
    #[error("debugger value has an unexpected shape; expected {expected}")]
    UnexpectedShape { expected: &'static str },
    #[error("failed to parse debugger scalar '{value}': {reason}")]
    InvalidScalar { value: String, reason: String },
    #[error("debugger dictionary is missing field '{0}'")]
    MissingField(String),
}

impl Dict {
    #[must_use]
    pub fn new(map: HashMap<String, Value>) -> Self {
        Self(map)
    }

    #[must_use]
    pub fn as_map(&self) -> &HashMap<String, Value> {
        &self.0
    }

    pub fn as_map_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.0
    }

    pub fn remove_expect(&mut self, key: &str) -> Result<Value, ValueError> {
        self.0
            .remove(key)
            .ok_or_else(|| ValueError::MissingField(key.to_string()))
    }
}

impl Value {
    pub fn get_dict_entry(&self, key: &str) -> Result<&Value, ValueError> {
        self.expect_dict_ref()?
            .get(key)
            .ok_or_else(|| ValueError::MissingField(key.to_string()))
    }

    pub fn expect_string(self) -> Result<String, ValueError> {
        match self {
            Self::String(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape { expected: "string" }),
        }
    }

    pub fn expect_string_ref(&self) -> Result<&str, ValueError> {
        match self {
            Self::String(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape { expected: "string" }),
        }
    }

    pub fn expect_string_repr<T>(&self) -> Result<T, ValueError>
    where
        T: std::str::FromStr,
        T::Err: Display,
    {
        let value = self.expect_string_ref()?;
        value
            .parse::<T>()
            .map_err(|error| ValueError::InvalidScalar {
                value: value.to_string(),
                reason: error.to_string(),
            })
    }

    pub fn expect_dict(self) -> Result<Dict, ValueError> {
        match self {
            Self::Dict(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape {
                expected: "dictionary",
            }),
        }
    }

    pub fn expect_dict_ref(&self) -> Result<&Dict, ValueError> {
        match self {
            Self::Dict(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape {
                expected: "dictionary",
            }),
        }
    }

    pub fn expect_dict_ref_mut(&mut self) -> Result<&mut Dict, ValueError> {
        match self {
            Self::Dict(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape {
                expected: "dictionary",
            }),
        }
    }

    pub fn expect_list(self) -> Result<List, ValueError> {
        match self {
            Self::List(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape { expected: "list" }),
        }
    }

    pub fn expect_list_ref(&self) -> Result<&List, ValueError> {
        match self {
            Self::List(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape { expected: "list" }),
        }
    }

    pub fn expect_list_ref_mut(&mut self) -> Result<&mut List, ValueError> {
        match self {
            Self::List(value) => Ok(value),
            _ => Err(ValueError::UnexpectedShape { expected: "list" }),
        }
    }
}

impl Deref for Dict {
    type Target = HashMap<String, Value>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Dict {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Index<&str> for Dict {
    type Output = Value;

    fn index(&self, key: &str) -> &Self::Output {
        self.get(key)
            .unwrap_or_else(|| panic!("key '{key}' not found in debugger dictionary"))
    }
}

impl IndexMut<&str> for Dict {
    fn index_mut(&mut self, key: &str) -> &mut Self::Output {
        self.get_mut(key)
            .unwrap_or_else(|| panic!("key '{key}' not found in debugger dictionary"))
    }
}

impl From<Dict> for Value {
    fn from(value: Dict) -> Self {
        Self::Dict(value)
    }
}

impl From<List> for Value {
    fn from(value: List) -> Self {
        Self::List(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<HashMap<String, Value>> for Value {
    fn from(value: HashMap<String, Value>) -> Self {
        Self::Dict(value.into())
    }
}

impl From<HashMap<&str, Value>> for Value {
    fn from(value: HashMap<&str, Value>) -> Self {
        Self::Dict(value.into())
    }
}

impl From<Vec<(String, Value)>> for Dict {
    fn from(value: Vec<(String, Value)>) -> Self {
        Self(value.into_iter().collect())
    }
}

impl From<HashMap<String, Value>> for Dict {
    fn from(value: HashMap<String, Value>) -> Self {
        Self(value)
    }
}

impl From<HashMap<&str, Value>> for Dict {
    fn from(value: HashMap<&str, Value>) -> Self {
        Self(
            value
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_values_support_checked_navigation() {
        let value = Value::Dict(HashMap::from([("pid".to_string(), Value::from("42"))]).into());

        assert_eq!(
            value
                .get_dict_entry("pid")
                .unwrap()
                .expect_string_repr::<u64>()
                .unwrap(),
            42
        );
        assert!(matches!(
            value.get_dict_entry("missing"),
            Err(ValueError::MissingField(field)) if field == "missing"
        ));
    }

    #[test]
    fn serialization_preserves_the_established_tagged_wire_shape() {
        let value = Value::List(vec![Value::from("one")]);
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::json!({"List": [{"String": "one"}]})
        );
    }
}
