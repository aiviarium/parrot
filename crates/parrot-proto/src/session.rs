//! Sessions and bindings (spec 9). `Task`, `Seed`, `Ceiling`, `Wake` and their requests are owned
//! by other modules and are carried as raw JSON here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::agent::Control;
use crate::ids::{
    AgentId, BindingId, Cursor, EnvId, ItemId, Meta, RuntimeId, SessionId, SubscriptionId, Timestamp, TurnId,
    WorkspaceId,
};
use crate::item::Origin;
use crate::open::Open;
use crate::option::PermissionLevel;
use crate::principal::{Isolation, Scope};
use crate::state::State;
use crate::usage::BudgetState;
use crate::workspace::WorkspaceRequest;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: SessionId,
    /// The owning runtime: where the current binding runs.
    pub runtime: RuntimeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<RuntimeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub workspace: WorkspaceId,
    pub cwd: String,
    pub state: State,
    pub background: Vec<Background>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<Binding>,
    pub bindings: Vec<BindingId>,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionParent>,
    /// A `Task` (19).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Value>,
    /// A `Seed` (23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<Value>,
    pub origin: Origin,
    pub grant: Vec<Scope>,
    /// A `Ceiling` (22).
    pub ceiling: Value,
    pub advisory: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Vec<McpServer>>,
    pub created: Timestamp,
    pub updated: Timestamp,
    pub archived: bool,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Root,
    Child,
    Embedded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionParent {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub name: String,
    pub status: McpStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    Connected,
    Failed,
    Pending,
    NeedsAuth,
}

/// A task's status as summaries carry it. `interrupted` comes from Appendix E7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Awaiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// What lists and the index feed carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: SessionId,
    pub runtime: RuntimeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: State,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Open<TaskStatus>>,
    pub archived: bool,
    pub updated: Timestamp,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: BindingId,
    pub session: SessionId,
    pub agent: AgentId,
    pub env: EnvId,
    pub control: Control,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Ref>,
    pub isolation: Isolation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<PermissionLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    pub status: BindingStatus,
    pub via: BindingVia,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<BindingPending>,
    pub started: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<Timestamp>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    Starting,
    Running,
    Stopped,
    Lost,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BindingVia {
    New,
    Resume,
    Adopt,
    Fork,
    Bundle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BindingPending {
    Bundle,
}

/// A harness's own reference to a session, item or request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Ref {
    pub id: String,
    pub strength: RefStrength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefStrength {
    Strong,
    Weak,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Background {
    pub id: String,
    pub kind: Open<BackgroundKind>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Task,
    Subagent,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnParentEnd {
    #[default]
    Continue,
    Stop,
}

/// `NewSession` in the spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewParams {
    pub agent: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
    /// Required, unless a `seed` that carries the workspace gives one (23.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<Vec<Scope>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// A `TaskRequest` (19).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Value>,
    /// A `SeedRequest` (23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<Value>,
    /// A `Ceiling` (22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<Value>,
    /// A `Wake` (20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<Value>,
    /// A `Budget` allotment (Appendix E9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<crate::usage::Budget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_parent_end: Option<OnParentEnd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub type NewSession = SessionNewParams;

/// The result of `session/new` and `session/adopt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewResult {
    pub session: Session,
    pub subscription: SubscriptionId,
    pub head: Cursor,
}

pub type SessionAdoptResult = SessionNewResult;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

/// Params of methods that take only `{ session }`: `session/get`, `session/delete`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRef {
    pub session: SessionId,
}

pub type SessionGetParams = SessionRef;
pub type SessionDeleteParams = SessionRef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionGetResult {
    pub session: Session,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeParams {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeResult {
    pub nodes: Vec<TreeNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub session: SessionSummary,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartParams {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// The result of `session/start` and `session/rebind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionBindingResult {
    pub binding: Binding,
}

pub type SessionStartResult = SessionBindingResult;
pub type SessionRebindResult = SessionBindingResult;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStopParams {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RebindRequire {
    Exact,
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRebindParams {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require: Option<RebindRequire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRewindParams {
    pub session: SessionId,
    pub turn: TurnId,
    pub files: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameParams {
    pub session: SessionId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionArchiveParams {
    pub session: SessionId,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDiscoverParams {
    pub agent: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDiscoverResult {
    pub found: Vec<DiscoveredSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

/// A harness session no Parrot session has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredSession {
    #[serde(rename = "ref")]
    pub reference: Ref,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub cwd: String,
    pub updated: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionAdoptParams {
    pub agent: AgentId,
    #[serde(rename = "ref")]
    pub reference: Ref,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// `session/configure` (Appendix E8): `delegate` on a subtree, `manage` on a root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigureParams {
    pub session: SessionId,
    /// A `Ceiling` (22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<crate::usage::Budget>,
    /// A `Wake` (20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_parent_end: Option<OnParentEnd>,
}
