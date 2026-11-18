//! The parts of opencode's HTTP API and event stream this driver reads.
//!
//! Every value keeps the fields this crate does not name, and an event this driver doesn't know is
//! still a `frame` update, so a newer opencode survives unchanged. The SSE frames carry the event as
//! a JSON string inside `data`; [`event`] unwraps that and leaves the raw object too.

use serde_json::{Map, Value, json};

/// One event from `/api/event`.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The event type, such as `session.text.delta`.
    pub kind: String,
    /// The event's own payload.
    pub data: Value,
    /// The whole event object, for a `frame` update.
    pub raw: Value,
}

/// Reads one SSE data payload. Some servers send the event object itself, others a JSON string.
pub fn event(value: &Value) -> Option<Event> {
    let owned;
    let value = match value {
        Value::String(text) => {
            owned = serde_json::from_str::<Value>(text).ok()?;
            &owned
        }
        other => other,
    };
    let kind = value.get("type").and_then(Value::as_str)?.to_owned();
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    Some(Event {
        kind,
        data,
        raw: value.clone(),
    })
}

/// A model as `GET /api/model` lists it, in the shape the options need.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// `provider/id`, the value of the `model` option.
    pub key: String,
    pub name: String,
    pub provider: String,
    /// The model id on its own, for `Model.Ref`.
    pub id: String,
    pub context: Option<u64>,
    /// Whether the model takes images.
    pub images: bool,
    pub enabled: bool,
}

impl Model {
    fn read(value: &Value) -> Option<Model> {
        let id = value.get("modelID").and_then(Value::as_str)?.to_owned();
        let provider = value
            .get("providerID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_owned();
        let context = value
            .get("limit")
            .and_then(|l| l.get("context"))
            .and_then(Value::as_u64);
        let images = value
            .get("capabilities")
            .and_then(|c| c.get("input"))
            .and_then(Value::as_array)
            .is_some_and(|inputs| inputs.iter().any(|i| i.as_str() == Some("image")));
        let enabled = value.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        Some(Model {
            key: format!("{provider}/{id}"),
            name,
            provider,
            id,
            context,
            images,
            enabled,
        })
    }
}

/// The models in a `GET /api/model` answer.
pub fn models(value: &Value) -> Vec<Model> {
    value
        .get("data")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Model::read).collect())
        .unwrap_or_default()
}

/// One provider from `GET /api/provider`.
#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
}

/// The providers in a `GET /api/provider` answer.
pub fn providers(value: &Value) -> Vec<Provider> {
    value
        .get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let id = p.get("id").and_then(Value::as_str)?.to_owned();
                    let name = p.get("name").and_then(Value::as_str).unwrap_or(&id).to_owned();
                    Some(Provider { id, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One command from `GET /api/command`.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub name: String,
    pub description: Option<String>,
}

/// The commands in a `GET /api/command` answer.
pub fn commands(value: &Value) -> Vec<Command> {
    value
        .get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let name = c.get("name").and_then(Value::as_str)?.to_owned();
                    let description = c.get("description").and_then(Value::as_str).map(str::to_owned);
                    Some(Command { name, description })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A tool result's content as text: a list of text and file parts.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// The `files` of a prompt: `uri` is all opencode needs, and a `data:` URI carries the bytes.
pub fn file_attachments(files: Vec<Value>) -> Value {
    Value::Array(files)
}

/// The body of `POST /api/session/{id}/prompt`.
pub fn prompt_body(text: &str, files: Vec<Value>, steer: bool) -> Value {
    let mut body = json!({"text": text});
    if !files.is_empty() {
        body["files"] = file_attachments(files);
    }
    if steer {
        body["delivery"] = json!("steer");
    }
    body
}

/// A `data:` URI for bytes a client sent, which is how opencode takes inline attachments.
pub fn data_uri(media_type: &str, data: &str) -> String {
    format!("data:{media_type};base64,{data}")
}

/// The `model` field of `POST /api/session` and `POST /api/session/{id}/model`.
pub fn model_ref(key: &str) -> Option<Value> {
    let (provider, id) = key.split_once('/')?;
    Some(json!({"providerID": provider, "id": id}))
}

/// Keeps the fields of an object this crate does not name.
pub fn extra(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_a_string_payload() {
        let outer = json!("{\"type\":\"session.idle\",\"data\":{\"sessionID\":\"s1\"}}");
        let e = event(&outer).expect("an event");
        assert_eq!(e.kind, "session.idle");
        assert_eq!(e.data["sessionID"], "s1");
        assert_eq!(e.raw["type"], "session.idle");
    }

    #[test]
    fn reads_models_and_their_capabilities() {
        let answer = json!({"data": [
            {"modelID": "m1", "providerID": "p", "name": "M One", "enabled": true,
             "capabilities": {"input": ["text", "image"]}, "limit": {"context": 1000, "output": 10}},
            {"modelID": "m2", "providerID": "p", "name": "M Two", "enabled": false,
             "capabilities": {"input": ["text"]}},
        ]});
        let list = models(&answer);
        assert_eq!(list[0].key, "p/m1");
        assert_eq!(list[0].context, Some(1000));
        assert!(list[0].images);
        assert!(!list[1].images);
        assert!(!list[1].enabled);
    }

    #[test]
    fn splits_a_model_ref() {
        assert_eq!(model_ref("p/m1"), Some(json!({"providerID": "p", "id": "m1"})));
        assert_eq!(model_ref("m1"), None);
    }
}
