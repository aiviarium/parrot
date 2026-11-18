//! The driver protocol (spec 30, Appendix E4): what the runtime asks a driver and what it reports.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::action::{Action, ActionStatus};
use crate::agent::Control;
use crate::ids::{AgentId, BindingId, Cursor, MessageId, Meta, RequestId, SessionId, TurnId};
use crate::item::ItemPatch;
use crate::open::Open;
use crate::option::{ConfigOption, PermissionLevel};
use crate::principal::Isolation;
use crate::prompt::{Attachment, ContextEntry};
use crate::request::{Answer, Request};
use crate::session::{Background, DiscoveredSession, McpServer, Ref};
use crate::state::{State, Turn};
use crate::subscribe::NoticeLevel;
use crate::usage::Usage;

pub use crate::agent::{AgentListParams as DriverAgentListParams, AgentListResult as DriverAgentListResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingStart {
    pub session: SessionId,
    pub binding: BindingId,
    pub agent: AgentId,
    /// The workspace's path.
    pub workspace: String,
    pub cwd: String,
    pub options: Map<String, Value>,
    pub control: Control,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<Pin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<Isolation>,
    /// Paths the harness must not reach.
    pub deny: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpInjection>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub type BindingStartParams = BindingStart;

/// Hold the harness to at most this level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    pub level: PermissionLevel,
    pub network: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpInjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingStartResult {
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Ref>,
    pub isolation: Isolation,
    pub level: PermissionLevel,
    pub network: bool,
    pub options: Vec<ConfigOption>,
    pub actions: Vec<Action>,
}

/// Params of `binding/stop` and `binding/screen`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingRef {
    pub binding: BindingId,
}

pub type BindingStopParams = BindingRef;
pub type BindingScreenParams = BindingRef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingPromptParams {
    pub binding: BindingId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    /// The id the user item gets.
    pub message: MessageId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<ContextEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    pub steer: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingPromptResult {
    pub disposition: BindingDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BindingDisposition {
    Started,
    Steered,
    Handled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingCancelParams {
    pub binding: BindingId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingCancelResult {
    pub interrupted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingAnswerParams {
    pub binding: BindingId,
    /// The driver key of the request.
    pub request: RequestId,
    pub answer: Answer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingOptionsParams {
    pub binding: BindingId,
    pub values: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingOptionsResult {
    pub options: Vec<ConfigOption>,
    pub level: PermissionLevel,
    pub network: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingActionParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// The user item id for turn actions (Appendix E4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingActionResult {
    pub status: ActionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingForkParams {
    pub binding: BindingId,
    /// A turn key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingForkResult {
    #[serde(rename = "ref")]
    pub reference: Ref,
}

pub type HistoryDiscoverParams = crate::session::SessionDiscoverParams;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDiscoverResult {
    pub found: Vec<DiscoveredSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryReadParams {
    pub agent: AgentId,
    #[serde(rename = "ref")]
    pub reference: Ref,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
}

/// The stored history as updates; each has `binding: ""` (Appendix E4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryReadResult {
    pub updates: Vec<DriverUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingScreenResult {
    pub text: String,
    pub cursor: ScreenPos,
    pub size: ScreenSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenPos {
    pub row: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenSize {
    pub rows: u32,
    pub cols: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingKeysParams {
    pub binding: BindingId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub press: Option<Vec<String>>,
}

/// Params of the driver's `update` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriverUpdate {
    /// Empty in `history/read` results.
    pub binding: BindingId,
    /// A subagent key (30.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    pub update: Open<DriverUpdateKind>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Ids inside are driver keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DriverUpdateKind {
    State {
        state: State,
        background: Vec<Background>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Item {
        item: ItemPatch,
    },
    Turn {
        turn: Turn,
    },
    Request {
        request: Box<Request>,
    },
    Options {
        options: Vec<ConfigOption>,
        level: PermissionLevel,
        network: bool,
    },
    Actions {
        actions: Vec<Action>,
    },
    Usage {
        usage: Usage,
    },
    Ref {
        #[serde(rename = "ref")]
        reference: Ref,
    },
    Mcp {
        servers: Vec<McpServer>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    Frame {
        source: String,
        data: Value,
    },
}
