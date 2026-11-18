//! The Agent Client Protocol, version 1, as a client of any ACP agent: JSON-RPC 2.0, one message
//! per line over the agent's stdio (Appendix A, Appendix C).
//!
//! Only the parts Parrot reads or writes are named. Every struct keeps unknown fields in
//! [`Extra`], and every session update is wrapped in [`Open`], so a newer agent's messages survive
//! a parse.

use parrot_proto::open::Open;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Fields this crate does not name, kept as they came.
pub type Extra = Map<String, Value>;

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// `initialize` params: Parrot offers no file system and no terminal, because the agent runs on
/// the same machine and uses its own.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
        },
        "clientInfo": {
            "name": "parrot",
            "title": "Parrot",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// What the agent said in `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(default)]
    pub protocol_version: Option<Value>,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub agent_info: Option<Value>,
    #[serde(default)]
    pub auth_methods: Vec<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Whether the agent takes images and embedded resources in a prompt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    /// Present when the agent can resume a session without replaying it.
    #[serde(default)]
    pub resume: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `session/new`, `session/load` and `session/resume` results.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResult {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub modes: Option<Modes>,
    #[serde(default)]
    pub models: Option<Models>,
    #[serde(default)]
    pub config_options: Option<Vec<ConfigOption>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Modes {
    #[serde(default)]
    pub current_mode_id: Option<String>,
    #[serde(default)]
    pub available_modes: Vec<ModeInfo>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The older, unstable `models` result shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Models {
    #[serde(default)]
    pub current_model_id: Option<String>,
    #[serde(default)]
    pub available_models: Vec<ModelInfo>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    #[serde(alias = "modelId", default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A select config option, such as the model or the thought level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOption {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub current_value: Value,
    #[serde(default)]
    pub options: Vec<ConfigChoice>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A choice of a config option; a choice may group more choices.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChoice {
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub options: Vec<ConfigChoice>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ConfigOption {
    /// The choices, with one level of groups flattened.
    pub fn flat_choices(&self) -> Vec<&ConfigChoice> {
        let mut out = Vec::new();
        for choice in &self.options {
            if choice.options.is_empty() {
                out.push(choice);
            } else {
                out.extend(choice.options.iter());
            }
        }
        out
    }
}

/// `session/prompt` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<PromptUsage>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cached_read_tokens: Option<u64>,
    #[serde(default)]
    pub cached_write_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A `session/update` notification's body, tagged by `sessionUpdate`.
pub type SessionUpdate = Open<SessionUpdateBody>;

/// The session updates Parrot maps. Others are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdateBody {
    AgentMessageChunk(ContentChunk),
    AgentThoughtChunk(ContentChunk),
    UserMessageChunk(ContentChunk),
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCall),
    Plan(Plan),
    AvailableCommandsUpdate(AvailableCommands),
    CurrentModeUpdate(CurrentMode),
    ConfigOptionUpdate(ConfigOptions),
    UsageUpdate(UsageUpdate),
    /// A free-text notice some agents send.
    Notice(Notice),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentChunk {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub content: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
    #[serde(default)]
    pub content: Option<Vec<Value>>,
    #[serde(default)]
    pub locations: Option<Vec<Location>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    #[serde(default)]
    pub entries: Vec<PlanEntry>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommands {
    #[serde(default)]
    pub available_commands: Vec<Command>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input: Option<CommandInput>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInput {
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentMode {
    #[serde(default)]
    pub current_mode_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOptions {
    #[serde(default)]
    pub config_options: Option<Vec<ConfigOption>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageUpdate {
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub cost: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `session/request_permission` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub tool_call: Option<ToolCall>,
    #[serde(default)]
    pub options: Vec<PermissionOption>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    #[serde(default)]
    pub option_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `session/request_permission`.
pub fn selected(option_id: &str) -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

/// The answer to a permission request the turn no longer needs.
pub fn cancelled_outcome() -> Value {
    json!({ "outcome": { "outcome": "cancelled" } })
}

/// The text of a content block, if it has any.
pub fn block_text(block: &Value) -> Option<&str> {
    (block.get("type").and_then(Value::as_str) == Some("text"))
        .then(|| block.get("text").and_then(Value::as_str))
        .flatten()
}

/// Drops the values that carry secrets before a raw message goes into a `frame` update: bearer
/// tokens, `Authorization` headers and fields named like secrets.
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let lower = k.to_ascii_lowercase();
                let secret = matches!(
                    lower.as_str(),
                    "authorization" | "token" | "access_token" | "api_key" | "apikey" | "secret" | "password"
                );
                if secret && v.is_string() {
                    out.insert(k.clone(), Value::String("[redacted]".into()));
                    continue;
                }
                match v {
                    Value::String(s) if s.starts_with("Bearer ") => {
                        out.insert(k.clone(), Value::String("[redacted]".into()));
                    }
                    _ => {
                        out.insert(k.clone(), redact(v));
                    }
                }
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_read_from_the_published_shape() {
        let v = json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": { "image": true, "embeddedContext": true },
                "mcpCapabilities": { "http": true, "sse": false },
                "sessionCapabilities": { "resume": {} },
            },
            "agentInfo": { "name": "fake-acp", "version": "0.0.0" },
            "authMethods": [],
        });
        let init: InitializeResult = serde_json::from_value(v).unwrap();
        assert!(init.agent_capabilities.load_session);
        assert!(init.agent_capabilities.prompt_capabilities.image);
        assert!(init.agent_capabilities.prompt_capabilities.embedded_context);
        assert!(init.agent_capabilities.mcp_capabilities.http);
        assert!(init.agent_capabilities.session_capabilities.resume.is_some());
    }

    #[test]
    fn updates_keep_unknown_kinds_raw() {
        let known: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "m1",
            "content": { "type": "text", "text": "hi" },
        }))
        .unwrap();
        assert!(matches!(
            known,
            Open::Known(SessionUpdateBody::AgentMessageChunk(_))
        ));
        let unknown: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "x.acme/new_thing",
            "value": 1,
        }))
        .unwrap();
        assert!(matches!(unknown, Open::Unknown(_)));
    }

    #[test]
    fn config_choices_flatten_one_group_level() {
        let option: ConfigOption = serde_json::from_value(json!({
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "small",
            "options": [
                { "value": "small", "name": "Small" },
                { "name": "More", "options": [{ "value": "large", "name": "Large" }] },
            ],
        }))
        .unwrap();
        let values: Vec<_> = option
            .flat_choices()
            .iter()
            .filter_map(|c| c.value.clone())
            .collect();
        assert_eq!(values, ["small", "large"]);
    }

    #[test]
    fn redaction_drops_tokens_and_bearer_headers() {
        let v = json!({
            "mcpServers": [{
                "type": "http",
                "url": "http://127.0.0.1:1/mcp",
                "headers": [{ "name": "Authorization", "value": "Bearer parrot_tok_1" }],
            }],
            "token": "parrot_tok_1",
        });
        let red = redact(&v);
        let text = red.to_string();
        assert!(!text.contains("parrot_tok_1"), "{text}");
        assert!(text.contains("[redacted]"));
    }
}
