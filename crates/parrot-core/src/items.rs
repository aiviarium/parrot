//! The item projection per session (spec 12), fed by every published item patch.

use crate::State;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Item projections by session.
#[derive(Debug, Default)]
pub struct Items {
    pub by_session: BTreeMap<String, SessionItems>,
}

/// One session's items in creation order.
#[derive(Debug, Default)]
pub struct SessionItems {
    pub order: Vec<String>,
    pub by_id: BTreeMap<String, Map<String, Value>>,
}

/// Apply a patch to the session's projection. A patch for an unknown item without `kind` is
/// dropped, since the first patch must create the item.
pub fn apply(state: &mut State, session: &str, patch: &Value) {
    let Some(p) = patch.as_object() else { return };
    let Some(id) = p.get("id").and_then(Value::as_str) else {
        return;
    };
    let items = state.items.by_session.entry(session.to_string()).or_default();
    if p.get("removed") == Some(&Value::Bool(true)) {
        items.by_id.remove(id);
        items.order.retain(|i| i != id);
        return;
    }
    if !items.by_id.contains_key(id) {
        if !p.contains_key("kind") {
            return;
        }
        items.order.push(id.to_string());
        items.by_id.insert(id.to_string(), Map::new());
    }
    let item = items.by_id.get_mut(id).expect("inserted above");
    apply_patch(item, p);
}

/// The patch rules of spec 12.1: header fields copy, `kind` never changes, `set` before `append`,
/// `_meta` in `set` merges by key.
pub fn apply_patch(item: &mut Map<String, Value>, p: &Map<String, Value>) {
    for f in [
        "id",
        "turn",
        "binding",
        "parent",
        "source",
        "ref",
        "origin",
        "streaming",
    ] {
        if let Some(v) = p.get(f) {
            item.insert(f.into(), v.clone());
        }
    }
    if let Some(kind) = p.get("kind") {
        item.entry("kind").or_insert_with(|| kind.clone());
    }
    if let Some(Value::Object(set)) = p.get("set") {
        for (k, v) in set {
            match (k.as_str(), v) {
                (_, Value::Null) => {
                    item.remove(k);
                }
                ("_meta", Value::Object(meta)) => {
                    let slot = item.entry("_meta").or_insert_with(|| Value::Object(Map::new()));
                    if !slot.is_object() {
                        *slot = Value::Object(Map::new());
                    }
                    let target = slot.as_object_mut().expect("an object");
                    for (mk, mv) in meta {
                        if mv.is_null() {
                            target.remove(mk);
                        } else {
                            target.insert(mk.clone(), mv.clone());
                        }
                    }
                }
                _ => {
                    item.insert(k.clone(), v.clone());
                }
            }
        }
    }
    if let Some(Value::Object(append)) = p.get("append") {
        for (k, v) in append {
            let Some(add) = v.as_str() else { continue };
            match item.get_mut(k) {
                Some(Value::String(s)) => s.push_str(add),
                Some(_) => {}
                None => {
                    item.insert(k.clone(), Value::String(add.to_string()));
                }
            }
        }
    }
}

/// One item, as the projection holds it.
pub fn get(state: &State, session: &str, item: &str) -> Option<Value> {
    state
        .items
        .by_session
        .get(session)
        .and_then(|s| s.by_id.get(item))
        .map(|m| Value::Object(m.clone()))
}

/// The newest `n` items, oldest first.
pub fn newest(state: &State, session: &str, n: usize) -> Vec<Value> {
    let Some(s) = state.items.by_session.get(session) else {
        return Vec::new();
    };
    let start = s.order.len().saturating_sub(n);
    s.order[start..]
        .iter()
        .filter_map(|id| s.by_id.get(id).map(|m| Value::Object(m.clone())))
        .collect()
}

/// Whether the projection holds an item.
pub fn contains(state: &State, session: &str, item: &str) -> bool {
    state
        .items
        .by_session
        .get(session)
        .is_some_and(|s| s.by_id.contains_key(item))
}
