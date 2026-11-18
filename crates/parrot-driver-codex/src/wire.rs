//! The Codex app-server protocol (`codex app-server`): JSON-RPC 2.0 without the `jsonrpc` field, one
//! JSON object per line over stdio. Checked against the schema of CLI 0.170.0
//! (`codex app-server generate-json-schema`) and lines recorded from CLI 0.167.1.
//!
//! Every struct keeps the fields it does not name in `extra`, and every tagged union is wrapped in
//! [`Open`], so a newer CLI's messages survive a parse unchanged.

use parrot_proto::open::Open;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Fields this crate does not name, kept as they came.
pub type Extra = Map<String, Value>;

/// A JSON-RPC id. Codex sends numbers (starting at 0) for its own requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Num(i64),
    Str(String),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Num(n) => write!(f, "{n}"),
            RequestId::Str(s) => f.write_str(s),
        }
    }
}

/// One line in either direction, by its envelope.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: RequestId,
        result: Value,
    },
    Error {
        id: RequestId,
        error: RpcError,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Message {
    /// Reads an envelope. Only a line that is not a JSON object with `method` or `id` fails.
    pub fn parse(line: &str) -> Result<Message, String> {
        let v: Value = serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
        Message::from_value(v)
    }

    pub fn from_value(mut v: Value) -> Result<Message, String> {
        let Some(o) = v.as_object_mut() else {
            return Err("not an object".into());
        };
        let id = o
            .remove("id")
            .map(serde_json::from_value::<RequestId>)
            .transpose()
            .map_err(|e| e.to_string())?;
        let params = o.remove("params").unwrap_or(Value::Null);
        match (o.remove("method"), id) {
            (Some(Value::String(method)), Some(id)) => Ok(Message::Request { id, method, params }),
            (Some(Value::String(method)), None) => Ok(Message::Notification { method, params }),
            (_, Some(id)) => {
                if let Some(e) = o.remove("error") {
                    let error = serde_json::from_value(e).map_err(|e| e.to_string())?;
                    Ok(Message::Error { id, error })
                } else {
                    Ok(Message::Response {
                        id,
                        result: o.remove("result").unwrap_or(Value::Null),
                    })
                }
            }
            _ => Err("neither a request, a notification nor a response".into()),
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Message::Request { id, method, params } => json!({"id": id, "method": method, "params": params}),
            Message::Notification { method, params } => json!({"method": method, "params": params}),
            Message::Response { id, result } => json!({"id": id, "result": result}),
            Message::Error { id, error } => json!({"id": id, "error": error}),
        }
    }

    /// One line, without the newline.
    pub fn to_line(&self) -> String {
        self.to_value().to_string()
    }
}

/// Notifications from the app-server that the mapping reads, by `method`, with their `params`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ServerNotification {
    #[serde(rename = "thread/started")]
    ThreadStarted(ThreadStarted),
    #[serde(rename = "turn/started")]
    TurnStarted(TurnNotification),
    #[serde(rename = "turn/completed")]
    TurnCompleted(TurnNotification),
    #[serde(rename = "item/started")]
    ItemStarted(ItemNotification),
    #[serde(rename = "item/completed")]
    ItemCompleted(ItemNotification),
    #[serde(rename = "item/agentMessage/delta")]
    AgentMessageDelta(Delta),
    #[serde(rename = "item/plan/delta")]
    PlanDelta(Delta),
    #[serde(rename = "item/reasoning/summaryTextDelta")]
    ReasoningSummaryDelta(Delta),
    #[serde(rename = "item/reasoning/textDelta")]
    ReasoningTextDelta(Delta),
    #[serde(rename = "item/reasoning/summaryPartAdded")]
    ReasoningSummaryPartAdded(Delta),
    #[serde(rename = "item/commandExecution/outputDelta")]
    CommandOutputDelta(Delta),
    #[serde(rename = "item/fileChange/outputDelta")]
    FileChangeOutputDelta(Delta),
    #[serde(rename = "turn/plan/updated")]
    TurnPlanUpdated(TurnPlanUpdated),
    #[serde(rename = "thread/tokenUsage/updated")]
    TokenUsageUpdated(TokenUsageUpdated),
    #[serde(rename = "account/rateLimits/updated")]
    RateLimitsUpdated(RateLimitsUpdated),
    #[serde(rename = "serverRequest/resolved")]
    ServerRequestResolved(ServerRequestResolved),
    #[serde(rename = "model/rerouted")]
    ModelRerouted(ModelRerouted),
    #[serde(rename = "error")]
    Error(ErrorNotification),
}

impl ServerNotification {
    pub fn parse(method: &str, params: &Value) -> Open<ServerNotification> {
        serde_json::from_value(json!({"method": method, "params": params}))
            .unwrap_or_else(|_| Open::Unknown(json!({"method": method, "params": params})))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStarted {
    pub thread: Thread,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    #[serde(default)]
    pub thread_id: Option<String>,
    pub turn: TurnInfo,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInfo {
    pub id: String,
    /// `inProgress`, `completed`, `interrupted` or `failed`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error: Option<TurnErrorInfo>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnErrorInfo {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemNotification {
    pub item: Open<ThreadItem>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    pub item_id: String,
    #[serde(default)]
    pub delta: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnPlanUpdated {
    pub turn_id: String,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub plan: Vec<PlanStep>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    #[serde(default)]
    pub step: String,
    /// `pending`, `inProgress` or `completed`.
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageUpdated {
    #[serde(default)]
    pub turn_id: Option<String>,
    pub token_usage: ThreadTokenUsage,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsage {
    #[serde(default)]
    pub total: TokenBreakdown,
    #[serde(default)]
    pub last: TokenBreakdown,
    #[serde(default)]
    pub model_context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub cache_write_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitsUpdated {
    pub rate_limits: RateLimitSnapshot,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    #[serde(default)]
    pub limit_id: Option<String>,
    #[serde(default)]
    pub limit_name: Option<String>,
    #[serde(default)]
    pub primary: Option<RateLimitWindow>,
    #[serde(default)]
    pub secondary: Option<RateLimitWindow>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    pub used_percent: f64,
    #[serde(default)]
    pub resets_at: Option<i64>,
    #[serde(default)]
    pub window_duration_mins: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestResolved {
    pub request_id: RequestId,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRerouted {
    #[serde(default)]
    pub to_model: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub error: TurnErrorInfo,
    #[serde(default)]
    pub will_retry: bool,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A `ThreadItem`, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    UserMessage(UserMessage),
    AgentMessage(TextItem),
    Reasoning(Reasoning),
    Plan(TextItem),
    CommandExecution(CommandExecution),
    FileChange(FileChange),
    McpToolCall(McpToolCall),
    DynamicToolCall(DynamicToolCall),
    WebSearch(WebSearch),
    ImageView(ImageView),
    CollabAgentToolCall(CollabAgentToolCall),
    EnteredReviewMode(ReviewItem),
    ExitedReviewMode(ReviewItem),
    ContextCompaction(Bare),
}

impl ThreadItem {
    pub fn id(&self) -> &str {
        match self {
            ThreadItem::UserMessage(i) => &i.id,
            ThreadItem::AgentMessage(i) | ThreadItem::Plan(i) => &i.id,
            ThreadItem::Reasoning(i) => &i.id,
            ThreadItem::CommandExecution(i) => &i.id,
            ThreadItem::FileChange(i) => &i.id,
            ThreadItem::McpToolCall(i) => &i.id,
            ThreadItem::DynamicToolCall(i) => &i.id,
            ThreadItem::WebSearch(i) => &i.id,
            ThreadItem::ImageView(i) => &i.id,
            ThreadItem::CollabAgentToolCall(i) => &i.id,
            ThreadItem::EnteredReviewMode(i) | ThreadItem::ExitedReviewMode(i) => &i.id,
            ThreadItem::ContextCompaction(i) => &i.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bare {
    pub id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub id: String,
    #[serde(default)]
    pub content: Vec<Value>,
    /// The `clientUserMessageId` given to `turn/start` or `turn/steer`.
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl UserMessage {
    /// The text parts, joined.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|c| c["type"] == "text")
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextItem {
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reasoning {
    pub id: String,
    #[serde(default)]
    pub summary: Vec<String>,
    #[serde(default)]
    pub content: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecution {
    pub id: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub aggregated_output: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub id: String,
    #[serde(default)]
    pub changes: Vec<FileUpdateChange>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUpdateChange {
    pub path: String,
    #[serde(default)]
    pub diff: Option<String>,
    /// `{type: "add" | "delete" | "update", move_path?}`.
    #[serde(default)]
    pub kind: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolCall {
    pub id: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCall {
    pub id: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub content_items: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearch {
    pub id: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub results: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageView {
    pub id: String,
    #[serde(default)]
    pub path: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabAgentToolCall {
    pub id: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub id: String,
    #[serde(default)]
    pub review: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A model from `model/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<EffortOption>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffortOption {
    pub reasoning_effort: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelList {
    #[serde(default)]
    pub data: Vec<Model>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Methods of the requests the app-server sends.
pub mod server_request {
    pub const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
    pub const FILE_CHANGE_APPROVAL: &str = "item/fileChange/requestApproval";
    pub const PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";
    pub const USER_INPUT: &str = "item/tool/requestUserInput";
    pub const ELICITATION: &str = "mcpServer/elicitation/request";
    pub const EXEC_APPROVAL_V1: &str = "execCommandApproval";
    pub const PATCH_APPROVAL_V1: &str = "applyPatchApproval";
}

/// `/bin/bash -lc 'echo hi'` → `echo hi`: the shell wrapper Codex puts around commands.
pub fn short_command(c: &str) -> String {
    for shell in ["/bin/bash -lc ", "/bin/zsh -lc ", "/bin/sh -lc ", "bash -lc "] {
        if let Some(rest) = c.strip_prefix(shell) {
            let q = rest.chars().next();
            if let Some(q @ ('\'' | '"')) = q
                && rest.len() >= 2
                && rest.ends_with(q)
            {
                let inner = &rest[1..rest.len() - 1];
                return if q == '\'' {
                    inner.replace("'\\''", "'")
                } else {
                    inner.to_owned()
                };
            }
            if !rest.is_empty() && !rest.contains(['\'', '"', ' ']) {
                return rest.to_owned();
            }
        }
    }
    c.to_owned()
}
