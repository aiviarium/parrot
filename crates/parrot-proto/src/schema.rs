//! The generated JSON Schema of the protocol (spec 38).
//!
//! `all()` builds the committed `spec/schema/parrot.schema.json`; add each new wire type with one
//! line in [`register`].

use schemars::{JsonSchema, SchemaGenerator};
use serde_json::{Map, Value, json};

use crate::error;

/// The schema of one type, with its own `$defs`.
pub fn schema_for<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schemas serialize")
}

/// Every protocol type, one line each. The types agent and the integrator extend this list.
pub fn register(r: &mut Registry) {
    r.add::<envelope::Message>();
    r.add::<envelope::RpcError>();
    r.add::<error::UnsupportedData>();
    r.add::<error::ForbiddenData>();
    r.add::<error::WrongStateData>();
    r.add::<error::NoSharedVersionData>();
    r.add::<error::DetailData>();
    r.add::<error::MovedData>();
    r.add::<error::IsolationData>();
}

/// Collects types into one set of `$defs`.
pub struct Registry {
    generator: SchemaGenerator,
    roots: Map<String, Value>,
}

impl Registry {
    pub fn add<T: JsonSchema>(&mut self) {
        let s = serde_json::to_value(self.generator.subschema_for::<T>()).expect("serialize");
        // Types schemars inlines rather than references are stored under their own name.
        if s.get("$ref").is_none() {
            self.roots.insert(T::schema_name().into_owned(), s);
        }
    }
}

/// One document holding every registered type under `$defs`.
pub fn all() -> Value {
    let mut r = Registry {
        generator: SchemaGenerator::default(),
        roots: Map::new(),
    };
    register(&mut r);
    let mut defs: Map<String, Value> = r.generator.definitions().clone();
    defs.extend(r.roots);
    defs.sort_keys();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Parrot protocol, version 1",
        "$defs": defs,
    })
}

/// Schema-only copies of the frozen `rpc` envelopes, which don't derive `JsonSchema`.
/// They must keep the same serde shape as `crate::rpc`.
mod envelope {
    use schemars::JsonSchema;
    use serde_json::Value;

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    pub struct Request {
        pub jsonrpc: String,
        pub id: Value,
        pub method: String,
        #[serde(default)]
        pub params: Option<Value>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    pub struct Notification {
        pub jsonrpc: String,
        pub method: String,
        #[serde(default)]
        pub params: Option<Value>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    pub struct Response {
        pub jsonrpc: String,
        pub id: Value,
        #[serde(default)]
        pub result: Option<Value>,
        #[serde(default)]
        pub error: Option<RpcError>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    pub struct RpcError {
        pub code: i64,
        pub message: String,
        #[serde(default)]
        pub data: Option<Value>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    #[serde(untagged)]
    pub enum Message {
        Request(Request),
        Response(Response),
        Notification(Notification),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// Fails when the committed schema is stale; `PARROT_BLESS=1` rewrites it.
    #[test]
    fn committed_schema_is_current() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/schema/parrot.schema.json");
        let fresh = serde_json::to_string_pretty(&super::all()).unwrap() + "\n";
        if std::env::var_os("PARROT_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed != fresh {
            let diff: Vec<String> = committed
                .lines()
                .zip(fresh.lines())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(20)
                .map(|(i, (a, b))| format!("line {}:\n- {a}\n+ {b}", i + 1))
                .collect();
            panic!(
                "spec/schema/parrot.schema.json is stale ({} vs {} lines); run with PARROT_BLESS=1\n{}",
                committed.lines().count(),
                fresh.lines().count(),
                diff.join("\n")
            );
        }
    }

    #[test]
    fn has_message() {
        let all = super::all();
        assert!(all["$defs"]["Message"].is_object());
        assert!(all["$defs"]["MovedData"].is_object());
        assert!(super::schema_for::<super::error::MovedData>()["properties"]["runtime"].is_object());
    }
}
