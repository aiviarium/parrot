//! Claude Code's stream-json protocol: `claude -p --input-format stream-json --output-format
//! stream-json --verbose --include-partial-messages`, one JSON object per line in each direction.
//!
//! Every type keeps the fields it does not name in `extra`, and every tagged union is wrapped in
//! [`Open`], so a newer CLI's messages and fields survive a parse and a write unchanged.

use parrot_proto::open::Open;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// Fields this crate does not name, kept as they came.
pub type Extra = Map<String, Value>;

/// A line the CLI writes.
pub type ClaudeOut = Open<ClaudeOutMsg>;
/// A line written to the CLI.
pub type ClaudeIn = Open<ClaudeInMsg>;

/// Parses one line the CLI wrote. Only invalid JSON fails; an unknown shape is `Open::Unknown`.
pub fn parse_line(line: &str) -> Result<ClaudeOut, serde_json::Error> {
    serde_json::from_str(line.trim())
}

/// Parses one line written to the CLI, as the fake does.
pub fn parse_in_line(line: &str) -> Result<ClaudeIn, serde_json::Error> {
    serde_json::from_str(line.trim())
}

/// One line for the CLI's stdin, without the newline.
pub fn to_line(msg: &ClaudeIn) -> String {
    serde_json::to_string(msg).expect("wire types always serialize")
}

/// One line as the CLI writes it, without the newline.
pub fn out_to_line(msg: &ClaudeOut) -> String {
    serde_json::to_string(msg).expect("wire types always serialize")
}

/// Reads a field that may be `null`, so `null` and absent stay apart on the way back out.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// Messages from the CLI, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClaudeOutMsg {
    System(Open<System>),
    Assistant(Assistant),
    User(User),
    StreamEvent(StreamEvent),
    Result(ResultMsg),
    ControlRequest(ControlRequest),
    ControlResponse(ControlResponse),
    ControlCancelRequest(ControlCancelRequest),
    KeepAlive(KeepAlive),
}

/// Messages to the CLI, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClaudeInMsg {
    User(User),
    ControlRequest(ControlRequest),
    ControlResponse(ControlResponse),
    ControlCancelRequest(ControlCancelRequest),
    KeepAlive(KeepAlive),
}

/// `system` messages, tagged by `subtype`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum System {
    Init(SystemInit),
    CompactBoundary(CompactBoundary),
}

/// Permission modes. A new one parses as `Open::Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
    DontAsk,
}

/// `system`/`init`: sent at the start of each turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemInit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "permissionMode", default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<Open<PermissionMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `system`/`compact_boundary`: the conversation was compacted here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactBoundary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_metadata: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A whole assistant message, sent once its content is final.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assistant {
    pub message: ApiMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The Task tool call this message belongs to, for a subagent's messages.
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_tool_use_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A user message: a prompt written to the CLI, or tool results the CLI reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub message: ApiMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_tool_use_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl User {
    /// A plain text prompt.
    pub fn prompt(text: impl Into<String>) -> Self {
        User {
            message: ApiMessage {
                id: None,
                role: "user".into(),
                model: None,
                content: Content::Text(text.into()),
                stop_reason: None,
                usage: None,
                extra: Extra::new(),
            },
            session_id: None,
            parent_tool_use_id: Some(None),
            uuid: None,
            extra: Extra::new(),
        }
    }
}

/// A Messages API message as the CLI forwards it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub content: Content,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_reason: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Message content: a bare string or a list of blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<Open<ContentBlock>>),
}

impl Content {
    /// The text of the message's text blocks, joined by newlines.
    pub fn text(&self) -> String {
        match self {
            Content::Text(t) => t.clone(),
            Content::Blocks(bs) => bs
                .iter()
                .filter_map(|b| match b.known() {
                    Some(ContentBlock::Text(t)) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// Content blocks, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextBlock),
    Thinking(ThinkingBlock),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    /// A string or a list of blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Token counts. The CLI adds more (cache creation detail, server tool use) under `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `stream_event`: a raw Messages API streaming event, sent with `--include-partial-messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    pub event: Open<Event>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_tool_use_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Streaming events, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    MessageStart {
        message: ApiMessage,
        #[serde(flatten)]
        extra: Extra,
    },
    ContentBlockStart {
        index: u64,
        content_block: Open<ContentBlock>,
        #[serde(flatten)]
        extra: Extra,
    },
    ContentBlockDelta {
        index: u64,
        delta: Open<Delta>,
        #[serde(flatten)]
        extra: Extra,
    },
    ContentBlockStop {
        index: u64,
        #[serde(flatten)]
        extra: Extra,
    },
    MessageDelta {
        delta: MessageDelta,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(flatten)]
        extra: Extra,
    },
    MessageStop {
        #[serde(flatten)]
        extra: Extra,
    },
}

/// Content block deltas, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta { text: String },
    ThinkingDelta { thinking: String },
    InputJsonDelta { partial_json: String },
    SignatureDelta { signature: String },
}

/// The body of `message_delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_reason: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_sequence: Option<Option<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultSubtype {
    Success,
    ErrorMaxTurns,
    ErrorDuringExecution,
    ErrorMaxBudgetUsd,
    ErrorMaxStructuredOutputRetries,
}

/// `result`: the end of a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultMsg {
    pub subtype: Open<ResultSubtype>,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_api_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_turns: Option<u64>,
    /// The final text, on success and on some errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// A JSON number, so an integer cost writes back as it came; see [`ResultMsg::cost_usd`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Per-model usage, keyed by model id, with `contextWindow` among the fields.
    #[serde(rename = "modelUsage", default, skip_serializing_if = "Option::is_none")]
    pub model_usage: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_denials: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ResultMsg {
    pub fn cost_usd(&self) -> Option<f64> {
        self.total_cost_usd.as_ref().and_then(serde_json::Number::as_f64)
    }
}

/// `control_request`, in either direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub request_id: String,
    pub request: Open<ControlRequestBody>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ControlRequest {
    pub fn new(request_id: impl Into<String>, body: ControlRequestBody) -> Self {
        ControlRequest {
            request_id: request_id.into(),
            request: Open::Known(body),
            extra: Extra::new(),
        }
    }
}

/// Control requests, tagged by `subtype`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequestBody {
    /// Host to CLI, first: registers hooks and SDK MCP servers, answered with commands and models.
    Initialize {
        #[serde(flatten)]
        extra: Extra,
    },
    /// CLI to host: may this tool run? Answered with a [`PermissionResult`].
    CanUseTool(CanUseTool),
    /// Host to CLI: stop the current turn.
    Interrupt {
        #[serde(flatten)]
        extra: Extra,
    },
    SetPermissionMode {
        mode: Open<PermissionMode>,
        #[serde(flatten)]
        extra: Extra,
    },
    SetModel {
        /// `None` goes back to the default model.
        #[serde(default)]
        model: Option<String>,
        #[serde(flatten)]
        extra: Extra,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanUseTool {
    pub tool_name: String,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_suggestions: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `can_use_tool`, carried in a successful control response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum PermissionResult {
    Allow {
        #[serde(rename = "updatedInput", default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
        #[serde(
            rename = "updatedPermissions",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        updated_permissions: Option<Vec<Value>>,
        #[serde(flatten)]
        extra: Extra,
    },
    Deny {
        #[serde(default)]
        message: String,
        /// Also stop the turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
        #[serde(flatten)]
        extra: Extra,
    },
}

impl PermissionResult {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("wire types always serialize")
    }
}

/// `control_response`, in either direction. The request id sits inside `response`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub response: Open<ControlResponseBody>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ControlResponse {
    pub fn success(request_id: impl Into<String>, response: Value) -> Self {
        ControlResponse {
            response: Open::Known(ControlResponseBody::Success {
                request_id: request_id.into(),
                response: Some(response),
                extra: Extra::new(),
            }),
            extra: Extra::new(),
        }
    }

    pub fn error(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        ControlResponse {
            response: Open::Known(ControlResponseBody::Error {
                request_id: request_id.into(),
                error: error.into(),
                extra: Extra::new(),
            }),
            extra: Extra::new(),
        }
    }

    /// The id of the request this answers, also for an unknown body that has one.
    pub fn request_id(&self) -> Option<&str> {
        match &self.response {
            Open::Known(ControlResponseBody::Success { request_id, .. })
            | Open::Known(ControlResponseBody::Error { request_id, .. }) => Some(request_id),
            Open::Unknown(v) => v.get("request_id").and_then(Value::as_str),
        }
    }
}

/// Control response bodies, tagged by `subtype`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlResponseBody {
    Success {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Value>,
        #[serde(flatten)]
        extra: Extra,
    },
    Error {
        request_id: String,
        #[serde(default)]
        error: String,
        #[serde(flatten)]
        extra: Extra,
    },
}

/// `control_cancel_request`: the sender no longer wants an answer to `request_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlCancelRequest {
    pub request_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `keep_alive`: no content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KeepAlive {
    #[serde(flatten)]
    pub extra: Extra,
}
