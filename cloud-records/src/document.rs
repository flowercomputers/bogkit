use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
pub const MAX_DOCUMENT_DEPTH: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct JsonDocument(Value);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error("record must be a JSON object")]
    ObjectRequired,
    #[error("record exceeds maximum nesting depth of {max_depth}")]
    TooDeep { max_depth: usize },
    #[error("record exceeds maximum encoded size of {max_bytes} bytes")]
    TooLarge { max_bytes: usize },
}

impl JsonDocument {
    pub fn try_from_value(value: Value) -> Result<Self, DocumentError> {
        if !value.is_object() {
            return Err(DocumentError::ObjectRequired);
        }
        if value_depth(&value) > MAX_DOCUMENT_DEPTH {
            return Err(DocumentError::TooDeep {
                max_depth: MAX_DOCUMENT_DEPTH,
            });
        }
        if value.to_string().len() > MAX_DOCUMENT_BYTES {
            return Err(DocumentError::TooLarge {
                max_bytes: MAX_DOCUMENT_BYTES,
            });
        }
        Ok(Self(value))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

fn value_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(value_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(value_depth).max().unwrap_or(0),
        _ => 0,
    }
}

impl Serialize for JsonDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            self.as_value().serialize(serializer)
        } else {
            serializer.serialize_str(&self.as_value().to_string())
        }
    }
}

impl<'de> Deserialize<'de> for JsonDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = if deserializer.is_human_readable() {
            Value::deserialize(deserializer)?
        } else {
            let encoded = String::deserialize(deserializer)?;
            serde_json::from_str(&encoded).map_err(D::Error::custom)?
        };
        Self::try_from_value(value).map_err(D::Error::custom)
    }
}

impl JsonSchema for JsonDocument {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("JsonDocument")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({"type": "object"})
    }
}
