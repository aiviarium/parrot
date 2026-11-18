//! pi's RPC protocol (`pi --mode rpc`): one JSON object per line in each direction.
//!
//! A command carries an `id` and a `type`; pi answers with `{"type":"response","id",...}` and sends
//! events as they happen. Every struct keeps the fields this crate does not name in `extra`, and
//! every inbound line that fails to parse is kept as raw JSON, so a newer pi survives unchanged.

use parrot_proto::open::Open;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Fields this crate does not name, kept as they came.
pub type Extra = Map<String, Value>;

/// One command line written to pi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub r#type: String,
    #[serde(flatten)]
    pub fields: Extra,
}

impl Command {
    pub fn new(id: impl Into<String>, kind: &str) -> Self {
        Command {
            id: id.into(),
            r#type: kind.to_owned(),
            fields: Extra::new(),
        }
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.fields.insert(key.to_owned(), value);
        self
    }

    pub fn prompt(id: impl Into<String>, message: &str, images: &[Image]) -> Self {
        let mut c = Command::new(id, "prompt").with("message", json!(message));
        if !images.is_empty() {
            c = c.with("images", json!(images));
        }
        c
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("a command serializes")
    }
}

/// An image as pi's `prompt` and `steer` take it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: String,
    pub mime_type: String,
}

impl Image {
    pub fn png(data: impl Into<String>) -> Self {
        Image {
            kind: "image".into(),
            data: data.into(),
            mime_type: "image/png".into(),
        }
    }
}

/// The answer to a command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Response {
    /// The id as the driver wrote it.
    pub fn key(&self) -> Option<String> {
        match self.id.as_ref()? {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }
}

/// A message pi reports in `message_start` and `message_end`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: Value,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RawUsage>,
    /// The calls a tool made to other tools, on a tool result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_calls: Option<Box<NestedCalls>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The bounded record of a tool's calls to other tools (`nestedCalls`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedCalls {
    #[serde(default)]
    pub calls: Vec<NestedCall>,
    #[serde(default)]
    pub complete: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedCall {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The text of a message's content, whether it is a string or content blocks.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// pi's counts for one assistant message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawUsage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cost: Option<RawCost>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RawCost {
    #[serde(default)]
    pub total: f64,
}

impl RawUsage {
    pub fn cost(&self) -> f64 {
        self.cost.as_ref().map(|c| c.total).unwrap_or(0.0)
    }
}

/// A part of the assistant message that is streaming now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AssistantEvent {
    TextStart {
        #[serde(default)]
        content_index: u64,
        #[serde(flatten)]
        extra: Extra,
    },
    TextDelta {
        #[serde(default)]
        content_index: u64,
        #[serde(default)]
        delta: String,
        #[serde(flatten)]
        extra: Extra,
    },
    TextEnd {
        #[serde(default)]
        content_index: u64,
        #[serde(default)]
        content: Value,
        #[serde(flatten)]
        extra: Extra,
    },
    ThinkingStart {
        #[serde(default)]
        content_index: u64,
        #[serde(flatten)]
        extra: Extra,
    },
    ThinkingDelta {
        #[serde(default)]
        content_index: u64,
        #[serde(default)]
        delta: String,
        #[serde(flatten)]
        extra: Extra,
    },
    ThinkingEnd {
        #[serde(default)]
        content_index: u64,
        #[serde(default)]
        content: Value,
        #[serde(flatten)]
        extra: Extra,
    },
    #[serde(other)]
    Other,
}

impl AssistantEvent {
    pub fn index(&self) -> u64 {
        match self {
            AssistantEvent::TextStart { content_index, .. }
            | AssistantEvent::TextDelta { content_index, .. }
            | AssistantEvent::TextEnd { content_index, .. }
            | AssistantEvent::ThinkingStart { content_index, .. }
            | AssistantEvent::ThinkingDelta { content_index, .. }
            | AssistantEvent::ThinkingEnd { content_index, .. } => *content_index,
            AssistantEvent::Other => 0,
        }
    }

    /// Whether the part is text (`assistant`) rather than reasoning.
    pub fn is_text(&self) -> bool {
        matches!(
            self,
            AssistantEvent::TextStart { .. }
                | AssistantEvent::TextDelta { .. }
                | AssistantEvent::TextEnd { .. }
        )
    }
}

/// One line from pi that is not a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Event {
    AgentStart {
        #[serde(flatten)]
        extra: Extra,
    },
    TurnStart {
        #[serde(flatten)]
        extra: Extra,
    },
    MessageStart {
        #[serde(default)]
        message: Message,
        #[serde(flatten)]
        extra: Extra,
    },
    MessageUpdate {
        #[serde(default)]
        assistant_message_event: Option<AssistantEvent>,
        #[serde(flatten)]
        extra: Extra,
    },
    MessageEnd {
        #[serde(default)]
        message: Message,
        #[serde(flatten)]
        extra: Extra,
    },
    ToolExecutionStart {
        #[serde(default)]
        tool_call_id: String,
        #[serde(default)]
        tool_name: String,
        #[serde(default)]
        args: Value,
        #[serde(flatten)]
        extra: Extra,
    },
    ToolExecutionUpdate {
        #[serde(default)]
        tool_call_id: String,
        #[serde(default)]
        partial_result: Value,
        #[serde(flatten)]
        extra: Extra,
    },
    ToolExecutionEnd {
        #[serde(default)]
        tool_call_id: String,
        #[serde(default)]
        result: Value,
        #[serde(default)]
        is_error: bool,
        #[serde(flatten)]
        extra: Extra,
    },
    AgentEnd {
        #[serde(flatten)]
        extra: Extra,
    },
    AgentSettled {
        #[serde(default)]
        aborted: bool,
        #[serde(flatten)]
        extra: Extra,
    },
    ExtensionUiRequest {
        #[serde(default)]
        id: String,
        #[serde(default)]
        method: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        options: Option<Vec<String>>,
        #[serde(flatten)]
        extra: Extra,
    },
    AutoRetryEnd {
        #[serde(default)]
        success: bool,
        #[serde(default)]
        final_error: Option<String>,
        #[serde(flatten)]
        extra: Extra,
    },
    CompactionEnd {
        #[serde(default)]
        result: Option<Value>,
        #[serde(flatten)]
        extra: Extra,
    },
}

/// One line from pi: an answer to a command, or an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Response(Response),
    Event(Event),
}

/// Parses one line in either direction, keeping what this crate does not know as raw JSON.
pub fn parse_inbound(line: &str) -> Option<Open<Inbound>> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) == Some("response") {
        return Some(match serde_json::from_value::<Response>(value.clone()) {
            Ok(r) => Open::Known(Inbound::Response(r)),
            Err(_) => Open::Unknown(value),
        });
    }
    Some(match serde_json::from_value::<Event>(value.clone()) {
        Ok(e) => Open::Known(Inbound::Event(e)),
        Err(_) => Open::Unknown(value),
    })
}

/// The data of `get_available_models`: the model list pi offers.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelList {
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub provider: String,
    /// The input kinds the model takes, `text` and `image` among them.
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Model {
    /// The value the `model` option carries: `provider/id`, as `set_model` splits it.
    pub fn key(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    pub fn takes(&self, kind: &str) -> bool {
        self.input.iter().any(|i| i == kind)
    }
}

/// The data of `get_available_thinking_levels`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LevelList {
    #[serde(default)]
    pub levels: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The data of `get_commands`: slash commands and skills pi knows.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandList {
    #[serde(default)]
    pub commands: Vec<SlashCommand>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `prompt`, `extension` or `skill`.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The data of `get_fork_messages`: user messages a fork can start from.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessages {
    #[serde(default)]
    pub messages: Vec<ForkMessage>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessage {
    #[serde(default)]
    pub entry_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The data of `get_state`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<StateModel>,
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub is_streaming: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StateModel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(flatten)]
    pub extra: Extra,
}

impl SessionState {
    /// The model as the `model` option carries it, when pi names a real provider.
    pub fn model_key(&self) -> Option<String> {
        let m = self.model.as_ref()?;
        if m.provider.is_empty() || m.provider == "unknown" {
            None
        } else {
            Some(format!("{}/{}", m.provider, m.id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_carry_their_fields() {
        let line = Command::prompt("parrot-1", "hi", &[Image::png("AAA")]).to_line();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], "parrot-1");
        assert_eq!(v["type"], "prompt");
        assert_eq!(v["images"][0]["mimeType"], "image/png");
    }

    #[test]
    fn unknown_events_survive() {
        let line = r#"{"type":"x.acme/new","id":"n1","detail":4}"#;
        let parsed = parse_inbound(line).unwrap();
        match parsed {
            Open::Unknown(v) => assert_eq!(v["detail"], 4),
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn events_keep_unnamed_fields() {
        let line = r#"{"type":"tool_execution_end","toolCallId":"c1","result":{"content":[]},"isError":true,"durationMs":3}"#;
        let Open::Known(Inbound::Event(Event::ToolExecutionEnd { extra, is_error, .. })) =
            parse_inbound(line).unwrap()
        else {
            panic!("not a tool end");
        };
        assert!(is_error);
        assert_eq!(extra["durationMs"], 3);
    }
}
