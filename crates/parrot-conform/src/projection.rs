//! What a client or runtime would show after the messages seen so far: items after their patches,
//! requests, turns, options, queue, state and usage, per session or binding.

use std::collections::HashMap;

use serde_json::{Map, Value};

/// A list of objects keyed by id, in the order each id first appeared.
#[derive(Debug, Default, Clone)]
struct Keyed {
    order: Vec<(String, String)>,
    by_key: HashMap<(String, String), Map<String, Value>>,
}

impl Keyed {
    fn slot(&mut self, scope: &str, id: &str) -> &mut Map<String, Value> {
        let key = (scope.to_string(), id.to_string());
        if !self.by_key.contains_key(&key) {
            self.order.push(key.clone());
        }
        self.by_key.entry(key).or_default()
    }

    fn list(&self, scope: Option<&str>, filter: &Value) -> Value {
        let keep = |obj: &Map<String, Value>| match filter.as_object() {
            Some(f) => f.iter().all(|(k, v)| obj.get(k) == Some(v)),
            None => true,
        };
        Value::Array(
            self.order
                .iter()
                .filter(|(s, _)| scope.is_none_or(|want| want == s))
                .filter_map(|k| self.by_key.get(k))
                .filter(|o| keep(o))
                .map(|o| Value::Object(o.clone()))
                .collect(),
        )
    }
}

/// The latest value per scope, with the order it was set in.
#[derive(Debug, Default, Clone)]
struct Latest(HashMap<String, (u64, Value)>);

impl Latest {
    fn set(&mut self, scope: &str, n: u64, v: Value) {
        self.0.insert(scope.to_string(), (n, v));
    }

    fn get(&self, scope: Option<&str>) -> Value {
        match scope {
            Some(s) => self.0.get(s).map(|(_, v)| v.clone()).unwrap_or(Value::Null),
            None => self
                .0
                .values()
                .max_by_key(|(n, _)| *n)
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Projection {
    n: u64,
    items: Keyed,
    requests: Keyed,
    turns: Keyed,
    state: Latest,
    options: Latest,
    level: Latest,
    queue: Latest,
    usage: Latest,
    /// Item patches that did not apply, as `(item id, reason)`.
    pub errors: Vec<(String, String)>,
}

impl Projection {
    /// Feed one message from the target.
    pub fn observe(&mut self, msg: &Value) {
        self.n += 1;
        if msg["method"] == "update" {
            let p = &msg["params"];
            let scope = p["session"]
                .as_str()
                .or_else(|| p["binding"].as_str())
                .unwrap_or("");
            self.update(scope, &p["update"]);
        } else if let Some(result) = msg.get("result") {
            self.seed(result);
        }
    }

    /// Takes in what a response says about sessions: `session/new`, `session/get` and snapshots.
    fn seed(&mut self, result: &Value) {
        if let Some(sessions) = result["snapshot"]["sessions"].as_array() {
            for snap in sessions {
                let Some(id) = snap["session"]["id"].as_str() else {
                    continue;
                };
                let id = id.to_string();
                self.session(&id, &snap["session"]);
                for item in snap["items"].as_array().into_iter().flatten() {
                    if let Some(item_id) = item["id"].as_str() {
                        let slot = self.items.slot(&id, item_id);
                        *slot = item.as_object().cloned().unwrap_or_default();
                    }
                }
                for req in snap["requests"].as_array().into_iter().flatten() {
                    self.request(&id, req);
                }
                let n = self.n;
                if snap["queue"].is_object() {
                    self.queue.set(&id, n, snap["queue"].clone());
                }
                if snap["options"].is_array() {
                    self.options.set(&id, n, snap["options"].clone());
                }
                if !snap["usage"].is_null() {
                    self.usage.set(&id, n, snap["usage"].clone());
                }
            }
        } else if let Some(id) = result["session"]["id"].as_str() {
            let id = id.to_string();
            self.session(&id, &result["session"]);
        }
    }

    fn session(&mut self, id: &str, session: &Value) {
        if let Some(state) = session.get("state") {
            self.state.set(id, self.n, state.clone());
        }
        if let Some(level) = session["binding"].get("level") {
            self.level.set(id, self.n, level.clone());
        }
    }

    fn request(&mut self, scope: &str, req: &Value) {
        if let Some(id) = req["id"].as_str() {
            *self.requests.slot(scope, id) = req.as_object().cloned().unwrap_or_default();
        }
    }

    fn update(&mut self, scope: &str, u: &Value) {
        let n = self.n;
        match u["type"].as_str() {
            Some("item") => {
                let patch = &u["item"];
                let Some(id) = patch["id"].as_str() else {
                    return;
                };
                let slot = self.items.slot(scope, id);
                if let Err(e) = parrot_proto::patch::apply(slot, patch) {
                    self.errors.push((id.to_string(), e.to_string()));
                }
            }
            Some("request") => self.request(scope, &u["request"]),
            Some("turn") => {
                if let Some(id) = u["turn"]["id"].as_str() {
                    *self.turns.slot(scope, id) = u["turn"].as_object().cloned().unwrap_or_default();
                }
            }
            Some("state") => self.state.set(scope, n, u["state"].clone()),
            Some("session") => self.session(scope, &u["session"]),
            Some("queue") => self.queue.set(scope, n, u["queue"].clone()),
            Some("usage") => self.usage.set(scope, n, u["usage"].clone()),
            Some("options") => {
                self.options.set(scope, n, u["options"].clone());
                if let Some(level) = u.get("level") {
                    self.level.set(scope, n, level.clone());
                }
            }
            _ => {}
        }
    }

    /// The projection a check names: `{ "items" | "requests" | "turns": true | <filter> }` or
    /// `{ "state" | "options" | "level" | "queue" | "usage": true }`, with an optional `session`
    /// (or `binding`) to narrow it. A filter keeps entries whose fields equal the filter's.
    pub fn query(&self, what: &Value) -> Value {
        let scope = what["session"].as_str().or_else(|| what["binding"].as_str());
        for (key, list) in [
            ("items", &self.items),
            ("requests", &self.requests),
            ("turns", &self.turns),
        ] {
            if let Some(filter) = what.get(key) {
                return list.list(scope, filter);
            }
        }
        for (key, latest) in [
            ("state", &self.state),
            ("options", &self.options),
            ("level", &self.level),
            ("queue", &self.queue),
            ("usage", &self.usage),
        ] {
            if what.get(key).is_some() {
                return latest.get(scope);
            }
        }
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(binding: &str, u: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "update", "params": {"binding": binding, "update": u}})
    }

    #[test]
    fn items_follow_patches_and_filters() {
        let mut p = Projection::default();
        p.observe(&update(
            "b",
            json!({"type": "item", "item": {"id": "u", "kind": "user", "set": {"text": "hi"}}}),
        ));
        p.observe(&update("b", json!({"type": "item", "item": {"id": "a", "kind": "assistant", "set": {"text": ""}, "streaming": true}})));
        p.observe(&update(
            "b",
            json!({"type": "item", "item": {"id": "a", "append": {"text": "Hel"}}}),
        ));
        p.observe(&update(
            "b",
            json!({"type": "item", "item": {"id": "a", "append": {"text": "lo"}, "streaming": false}}),
        ));
        p.observe(&update(
            "b",
            json!({"type": "state", "state": "idle", "background": []}),
        ));
        let items = p.query(&json!({"items": {"kind": "assistant"}}));
        assert_eq!(items[0]["text"], "Hello");
        assert_eq!(items.as_array().unwrap().len(), 1);
        assert_eq!(p.query(&json!({"items": true})).as_array().unwrap().len(), 2);
        assert_eq!(p.query(&json!({"state": true, "binding": "b"})), "idle");
        assert_eq!(p.query(&json!({"state": true, "binding": "other"})), Value::Null);
    }

    #[test]
    fn snapshots_seed_sessions() {
        let mut p = Projection::default();
        p.observe(
            &json!({"jsonrpc": "2.0", "id": 3, "result": {"snapshot": {"sessions": [{
                "session": {"id": "ses_1", "state": "idle"},
                "items": [{"id": "i1", "kind": "assistant", "text": "old"}],
                "requests": [], "queue": {"paused": false, "entries": []}, "options": []
            }]}}}),
        );
        assert_eq!(p.query(&json!({"state": true})), "idle");
        assert_eq!(
            p.query(&json!({"items": true, "session": "ses_1"}))[0]["text"],
            "old"
        );
        assert_eq!(p.query(&json!({"queue": true}))["paused"], false);
    }
}
