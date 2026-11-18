//! The Agent Client Protocol, as a client of any ACP agent: JSON-RPC 2.0, one message per line
//! over the agent's stdio (Appendix A, Appendix C).
//!
//! `initialize` offers version 2 and falls back to version 1 for an agent that answers 1, as the
//! protocol asks. The v2 draft renames `clientCapabilities`, `clientInfo`, `agentCapabilities` and
//! `agentInfo` to `capabilities` and `info`; both spellings are sent and read, since an agent
//! ignores what it does not know.
//!
//! Only the parts Parrot reads or writes are named. Every struct keeps unknown fields in
//! [`Extra`], and every session update is wrapped in [`Open`], so a newer agent's messages survive
//! a parse.

use parrot_proto::open::Open;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Fields this crate does not name, kept as they came.
pub type Extra = Map<String, Value>;

/// The newest protocol version this client speaks. Version 2 is a draft; an agent that answers
/// 1 is served with the version 1 shapes.
pub const PROTOCOL_VERSION: u32 = 2;

/// The version this client falls back to when the agent names no version.
pub const PROTOCOL_V1: u32 = 1;

/// `initialize` params: Parrot offers no file system and no terminal, because the agent runs on
/// the same machine and uses its own.
pub fn initialize_params(version: u32) -> Value {
    let capabilities = json!({
        "fs": { "readTextFile": false, "writeTextFile": false },
        "terminal": false,
    });
    let info = json!({
        "name": "parrot",
        "title": "Parrot",
        "version": env!("CARGO_PKG_VERSION"),
    });
    json!({
        "protocolVersion": version,
        // The version 1 names and the version 2 names, so either agent reads its own.
        "clientCapabilities": capabilities,
        "clientInfo": info,
        "capabilities": capabilities,
        "info": info,
    })
}

/// What the agent said in `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(default)]
    pub protocol_version: Option<Value>,
    #[serde(default, alias = "capabilities")]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default, alias = "info")]
    pub agent_info: Option<Value>,
    #[serde(default)]
    pub auth_methods: Vec<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl InitializeResult {
    /// The protocol version the agent will speak: its answer, else version 1.
    pub fn version(&self) -> u32 {
        match &self.protocol_version {
            Some(Value::Number(n)) => n.as_u64().and_then(|v| u32::try_from(v).ok()).unwrap_or(PROTOCOL_V1),
            Some(Value::String(s)) => s.parse().unwrap_or(PROTOCOL_V1),
            _ => PROTOCOL_V1,
        }
    }

    /// The agent's version for `harness`, when it reports one.
    pub fn agent_version(&self) -> Option<String> {
        let info = self.agent_info.as_ref()?;
        info.get("version")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    }
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

/// The text of a content block, if the block carries or names any.
///
/// A `resource` block is decoded: its `text` as it is, or its base64 `blob` when the media type is
/// textual. A binary resource becomes a short description instead of its bytes. A `resource_link`
/// becomes the name and URI it names.
pub fn block_text(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => block.get("text").and_then(Value::as_str).map(str::to_owned),
        "resource" => Some(resource_text(block.get("resource")?)),
        "resource_link" => {
            let name = block.get("name").and_then(Value::as_str);
            let uri = block.get("uri").and_then(Value::as_str);
            match (name, uri) {
                (Some(name), Some(uri)) => Some(format!("{name}: {uri}")),
                (Some(name), None) => Some(name.to_owned()),
                (None, Some(uri)) => Some(uri.to_owned()),
                (None, None) => None,
            }
        }
        "image" => Some(image_text("image", block)),
        "audio" => Some(image_text("audio", block)),
        _ => None,
    }
}

/// The text an embedded resource carries, or a description of it when it carries bytes.
fn resource_text(resource: &Value) -> String {
    let uri = resource.get("uri").and_then(Value::as_str);
    if let Some(text) = resource.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    let mime = resource
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    match resource.get("blob").and_then(Value::as_str) {
        Some(blob) if mime.starts_with("text/") || mime == "application/json" => decode_base64(blob),
        Some(blob) => format!(
            "[{} resource{}: {} bytes]",
            mime,
            uri.map(|u| format!(" {u}")).unwrap_or_default(),
            decoded_len(blob)
        ),
        None => uri.unwrap_or("[resource]").to_owned(),
    }
}

fn image_text(kind: &str, block: &Value) -> String {
    let mime = block
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    let bytes = block
        .get("data")
        .and_then(Value::as_str)
        .map(decoded_len)
        .unwrap_or(0);
    format!("[{kind} {mime}: {bytes} bytes]")
}

/// How many bytes a base64 string holds, without decoding it.
fn decoded_len(data: &str) -> usize {
    let padding = data.bytes().rev().take_while(|b| *b == b'=').count();
    data.len() / 4 * 3 - padding.min(2)
}

/// Decodes base64 for an embedded resource; invalid input is kept as it came.
pub fn decode_base64(data: &str) -> String {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for c in data.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
