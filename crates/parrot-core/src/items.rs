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
    /// The `seq` each item was created at, to restore the order after a restart.
    pub created: BTreeMap<String, u64>,
}

/// Apply a patch to the session's projection and return the item as it now stands, or `None`
/// when the patch removed it or was dropped. A patch for an unknown item without `kind` is
/// dropped, since the first patch must create the item; so is one `parrot_proto::patch::apply`
/// refuses. `seq` is the update's sequence number, kept as the item's creation order.
pub fn apply(state: &mut State, session: &str, patch: &Value, seq: u64) -> Option<Value> {
    let p = patch.as_object()?;
    let id = p.get("id").and_then(Value::as_str)?;
    let items = state.items.by_session.entry(session.to_string()).or_default();
    if p.get("removed") == Some(&Value::Bool(true)) {
        items.by_id.remove(id);
        items.created.remove(id);
        items.order.retain(|i| i != id);
        return None;
    }
    let mut item = match items.by_id.get(id) {
        Some(item) => item.clone(),
        None if p.contains_key("kind") => Map::new(),
        None => return None,
    };
    parrot_proto::patch::apply(&mut item, patch).ok()?;
    if !items.by_id.contains_key(id) {
        items.order.push(id.to_string());
        items.created.insert(id.to_string(), seq);
    }
    items.by_id.insert(id.to_string(), item.clone());
    Some(Value::Object(item))
}

/// Put back an item read from the store, created at `seq`.
pub fn restore(state: &mut State, session: &str, item: Map<String, Value>, seq: u64) {
    let Some(id) = item.get("id").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let items = state.items.by_session.entry(session.to_string()).or_default();
    if !items.by_id.contains_key(&id) {
        items.order.push(id.clone());
    }
    items.created.insert(id.clone(), seq);
    items.by_id.insert(id, item);
}

/// The longest `output` shown in reads and snapshots, in bytes. `item/get` returns all of it.
pub const OUTPUT_LIMIT: usize = 64 * 1024;

/// An item as reads and snapshots show it: a long `output` is cut at a character boundary, and
/// `_meta["x.parrot/truncated"]` gives its full size in bytes (spec 12.2).
pub fn shown(mut item: Value) -> Value {
    let Some(full) = item.get("output").and_then(Value::as_str).map(str::len) else {
        return item;
    };
    if full <= OUTPUT_LIMIT {
        return item;
    }
    let text = item["output"].as_str().unwrap_or_default();
    let mut cut = OUTPUT_LIMIT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    item["output"] = Value::String(text[..cut].to_string());
    match item.get_mut("_meta").and_then(Value::as_object_mut) {
        Some(meta) => {
            meta.insert("x.parrot/truncated".into(), full.into());
        }
        None => item["_meta"] = serde_json::json!({ "x.parrot/truncated": full }),
    }
    item
}

/// When an item was created, as the `seq` of its first patch.
pub fn created(state: &State, session: &str, item: &str) -> u64 {
    state
        .items
        .by_session
        .get(session)
        .and_then(|s| s.created.get(item))
        .copied()
        .unwrap_or(0)
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

/// One page of `session/read` (spec 10.5). Cursors are item ids: reading back returns the items
/// before the cursor (or the newest without one), reading forward those after it, oldest first in
/// both directions. `before` and `after` are set when more items lie that way.
pub fn read(state: &State, session: &str, cursor: Option<&str>, forward: bool, limit: usize) -> Value {
    let order: &[String] = state
        .items
        .by_session
        .get(session)
        .map(|s| s.order.as_slice())
        .unwrap_or(&[]);
    let at = cursor.and_then(|c| order.iter().position(|i| i == c));
    let (start, end) = if forward {
        let start = match (cursor, at) {
            (Some(_), Some(i)) => i + 1,
            (Some(_), None) => order.len(),
            (None, _) => 0,
        };
        (start, (start + limit).min(order.len()))
    } else {
        let end = match (cursor, at) {
            (Some(_), Some(i)) => i,
            (Some(_), None) => 0,
            (None, _) => order.len(),
        };
        (end.saturating_sub(limit), end)
    };
    let page = &order[start..end];
    let s = state.items.by_session.get(session);
    let items: Vec<Value> = page
        .iter()
        .filter_map(|id| {
            s.and_then(|s| s.by_id.get(id))
                .map(|m| shown(Value::Object(m.clone())))
        })
        .collect();
    let mut out = serde_json::json!({ "items": items });
    if start > 0
        && let Some(first) = page.first().or(order.get(start - 1))
    {
        out["before"] = Value::String(first.clone());
    }
    if end < order.len()
        && let Some(last) = page.last().or(order.get(end))
    {
        out["after"] = Value::String(last.clone());
    }
    out
}
