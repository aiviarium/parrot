//! Item patches (spec 12): `set`, `append`, `streaming` and `removed`, applied to a JSON object.
//!
//! The rules live in one function so the runtime, clients and conformance agree.

use serde_json::{Map, Value};

/// Patch fields copied onto the item as they are, when present.
const HEADER_FIELDS: [&str; 7] = ["turn", "binding", "parent", "source", "ref", "origin", "id"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    #[error("a patch must be a JSON object")]
    NotAnObject,
    #[error("patch field `{0}` has the wrong type")]
    BadField(&'static str),
    #[error("patch id `{patch}` does not match item id `{item}`")]
    IdMismatch { item: String, patch: String },
    #[error("an item's kind never changes (`{from}` to `{to}`)")]
    KindChanged { from: String, to: String },
    #[error("the item was removed")]
    Removed,
    #[error("cannot append to field `{0}`, which is not a string")]
    AppendToNonString(String),
}

/// Apply one patch to an item held as a JSON object.
///
/// The item stores the header fields (`id`, `kind`, `turn`, ...) beside its kind's fields, and
/// `streaming` and `removed` as plain booleans. `set` applies before `append`.
pub fn apply(item: &mut Map<String, Value>, patch: &Value) -> Result<(), PatchError> {
    let p = patch.as_object().ok_or(PatchError::NotAnObject)?;
    if item.get("removed") == Some(&Value::Bool(true)) {
        return Err(PatchError::Removed);
    }
    if let (Some(Value::String(a)), Some(Value::String(b))) = (item.get("id"), p.get("id"))
        && a != b
    {
        return Err(PatchError::IdMismatch {
            item: a.clone(),
            patch: b.clone(),
        });
    }
    if let Some(kind) = p.get("kind") {
        let to = kind.as_str().ok_or(PatchError::BadField("kind"))?;
        match item.get("kind").and_then(Value::as_str) {
            Some(from) if from != to => {
                return Err(PatchError::KindChanged {
                    from: from.into(),
                    to: to.into(),
                });
            }
            Some(_) => {}
            None => {
                item.insert("kind".into(), kind.clone());
            }
        }
    }
    for f in HEADER_FIELDS {
        if let Some(v) = p.get(f) {
            item.insert(f.into(), v.clone());
        }
    }
    if let Some(set) = p.get("set") {
        let set = set.as_object().ok_or(PatchError::BadField("set"))?;
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
                    let target = slot.as_object_mut().expect("made an object above");
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
    if let Some(append) = p.get("append") {
        let append = append.as_object().ok_or(PatchError::BadField("append"))?;
        for (k, v) in append {
            let add = v.as_str().ok_or(PatchError::BadField("append"))?;
            match item
                .entry(k.clone())
                .or_insert_with(|| Value::String(String::new()))
            {
                Value::String(s) => s.push_str(add),
                _ => return Err(PatchError::AppendToNonString(k.clone())),
            }
        }
    }
    if let Some(s) = p.get("streaming") {
        let s = s.as_bool().ok_or(PatchError::BadField("streaming"))?;
        item.insert("streaming".into(), Value::Bool(s));
    }
    if let Some(r) = p.get("removed") {
        if r != &Value::Bool(true) {
            return Err(PatchError::BadField("removed"));
        }
        item.insert("removed".into(), Value::Bool(true));
    }
    Ok(())
}

/// True for a patch that only appends text, optionally ending the stream.
fn is_append_only(p: &Map<String, Value>) -> bool {
    p.contains_key("append")
        && p.keys()
            .all(|k| matches!(k.as_str(), "id" | "append" | "streaming"))
}

/// Fold each run of append-only patches to one item into a single `set` patch (spec 12.1, 35).
///
/// `patches` is a log in order, starting from each item's first patch. The folded patch sets the
/// appended fields to their full text at the end of the run and takes the place, and so the `seq`,
/// of the run's last patch. Applying the result from empty items gives the same items as the input.
pub fn consolidate(patches: &[Value]) -> Result<Vec<Value>, PatchError> {
    let mut items: Map<String, Value> = Map::new();
    let mut out: Vec<Value> = Vec::with_capacity(patches.len());
    // The open run: its item id, the fields it appended, and its last `streaming` value.
    let mut run: Option<(String, Vec<String>, Option<Value>)> = None;

    fn flush(
        run: &mut Option<(String, Vec<String>, Option<Value>)>,
        items: &Map<String, Value>,
        out: &mut Vec<Value>,
    ) {
        if let Some((id, fields, streaming)) = run.take() {
            let item = items.get(&id).and_then(Value::as_object);
            let mut set = Map::new();
            for f in fields {
                let v = item.and_then(|i| i.get(&f)).cloned().unwrap_or_default();
                set.insert(f, v);
            }
            let mut p = Map::new();
            p.insert("id".into(), Value::String(id));
            p.insert("set".into(), Value::Object(set));
            if let Some(s) = streaming {
                p.insert("streaming".into(), s);
            }
            out.push(Value::Object(p));
        }
    }

    for patch in patches {
        let p = patch.as_object().ok_or(PatchError::NotAnObject)?;
        let id = p
            .get("id")
            .and_then(Value::as_str)
            .ok_or(PatchError::BadField("id"))?
            .to_string();
        let item = items
            .entry(id.clone())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("items are objects");
        apply(item, patch)?;

        if is_append_only(p) {
            if run.as_ref().is_some_and(|(rid, _, _)| rid != &id) {
                flush(&mut run, &items, &mut out);
            }
            let (_, fields, streaming) = run.get_or_insert_with(|| (id, Vec::new(), None));
            if let Some(Value::Object(a)) = p.get("append") {
                for k in a.keys() {
                    if !fields.contains(k) {
                        fields.push(k.clone());
                    }
                }
            }
            if let Some(s) = p.get("streaming") {
                *streaming = Some(s.clone());
            }
        } else {
            flush(&mut run, &items, &mut out);
            out.push(patch.clone());
        }
    }
    flush(&mut run, &items, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn set_append_and_flags() {
        let mut item = Map::new();
        apply(
            &mut item,
            &json!({"id": "i1", "kind": "assistant", "set": {"text": "he"}, "streaming": true}),
        )
        .unwrap();
        apply(&mut item, &json!({"id": "i1", "append": {"text": "llo"}})).unwrap();
        apply(
            &mut item,
            &json!({"id": "i1", "set": {"_meta": {"a": 1}}, "streaming": false}),
        )
        .unwrap();
        apply(
            &mut item,
            &json!({"id": "i1", "set": {"_meta": {"b": 2}, "text": null}}),
        )
        .unwrap();
        assert_eq!(
            item,
            obj(json!({"id": "i1", "kind": "assistant", "streaming": false, "_meta": {"a": 1, "b": 2}}))
        );
        apply(&mut item, &json!({"id": "i1", "removed": true})).unwrap();
        assert_eq!(
            apply(&mut item, &json!({"id": "i1", "set": {}})),
            Err(PatchError::Removed)
        );
    }

    #[test]
    fn kind_is_fixed() {
        let mut item = obj(json!({"id": "i1", "kind": "tool"}));
        assert!(matches!(
            apply(&mut item, &json!({"id": "i1", "kind": "assistant"})),
            Err(PatchError::KindChanged { .. })
        ));
    }

    #[test]
    fn consolidates_a_run() {
        let log = vec![
            json!({"id": "i1", "kind": "assistant", "set": {"text": "a"}, "streaming": true}),
            json!({"id": "i1", "append": {"text": "b"}}),
            json!({"id": "i1", "append": {"text": "c"}, "streaming": false}),
            json!({"id": "i1", "set": {"done": true}}),
        ];
        let out = consolidate(&log).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[1],
            json!({"id": "i1", "set": {"text": "abc"}, "streaming": false})
        );
    }
}
