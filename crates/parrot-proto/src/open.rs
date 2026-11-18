//! Open unions (spec 38): a known value, or whatever a newer peer sent, kept as raw JSON.
//!
//! `#[serde(other)]` only covers unit variants, so unknown tagged objects need this wrapper.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Open<T> {
    Known(T),
    Unknown(Value),
}

impl<T> Open<T> {
    pub fn known(&self) -> Option<&T> {
        match self {
            Open::Known(t) => Some(t),
            Open::Unknown(_) => None,
        }
    }
}

impl<T> From<T> for Open<T> {
    fn from(t: T) -> Self {
        Open::Known(t)
    }
}

impl<T: Serialize> Serialize for Open<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Open::Known(t) => t.serialize(s),
            Open::Unknown(v) => v.serialize(s),
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for Open<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(match serde_json::from_value::<T>(v.clone()) {
            Ok(t) => Open::Known(t),
            Err(_) => Open::Unknown(v),
        })
    }
}

impl<T: JsonSchema> JsonSchema for Open<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Shape {
        Circle { r: f64 },
    }

    #[test]
    fn keeps_unknown_variants() {
        let known: Open<Shape> = serde_json::from_str(r#"{"type":"circle","r":1.0}"#).unwrap();
        assert_eq!(known, Open::Known(Shape::Circle { r: 1.0 }));
        let raw = r#"{"type":"x.acme/blob","n":2}"#;
        let unknown: Open<Shape> = serde_json::from_str(raw).unwrap();
        assert!(matches!(unknown, Open::Unknown(_)));
        assert_eq!(
            serde_json::to_string(&unknown).unwrap(),
            r#"{"type":"x.acme/blob","n":2}"#
        );
    }
}
