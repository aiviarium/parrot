//! Item patches (spec 12): `set`, `append`, `streaming` and `removed`, applied to a JSON object.
//!
//! The rules live in one function so the runtime, clients and conformance agree.

use serde_json::{Map, Value};

/// Apply one patch's fields to an item held as a JSON object. Implemented with the patch types.
pub fn apply(_item: &mut Map<String, Value>, _patch: &Value) {}
