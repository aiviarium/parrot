//! Items and item patches (spec 12). Applying patches lives in `patch.rs`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{BindingId, ItemId, MessageId, Meta, SessionId, TurnId};
use crate::open::Open;
use crate::principal::Principal;
use crate::prompt::{Attachment, ContextEntry};
use crate::session::{BindingVia, Ref};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemPatch {
    pub id: ItemId,
    /// Required on the first patch; never changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Open<ItemKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ItemSource>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Replace fields; `null` removes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<Map<String, Value>>,
    /// Append to string fields, after `set`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// Always `true` when present: the item is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<bool>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Script,
    Command,
    FileChange,
    Plan,
    Subagent,
    Web,
    Review,
    Compaction,
    Hook,
    Handoff,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemSource {
    Native,
    Transcript,
    Tui,
    Inferred,
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Origin {
    pub kind: Open<OriginKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<Principal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<OriginCause>,
    /// The well-known action that produced the item, such as `continue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Client,
    Harness,
    Remote,
    Hook,
    Session,
    Wake,
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OriginCause {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
}

/// A whole item. Unknown kinds stay raw JSON.
pub type Item = Open<ItemBody>;

/// Fields every item kind has.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemCommon {
    pub id: ItemId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ItemSource>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    User(UserItem),
    Assistant(TextItem),
    Reasoning(ReasoningItem),
    Tool(ToolItem),
    Script(ScriptItem),
    Command(CommandItem),
    FileChange(FileChangeItem),
    Plan(PlanItem),
    Subagent(SubagentItem),
    Web(WebItem),
    Review(ReviewItem),
    Compaction(CompactionItem),
    Hook(HookItem),
    Handoff(HandoffItem),
    Error(ErrorItem),
}

impl ItemBody {
    pub fn common(&self) -> &ItemCommon {
        match self {
            ItemBody::User(i) => &i.common,
            ItemBody::Assistant(i) => &i.common,
            ItemBody::Reasoning(i) => &i.common,
            ItemBody::Tool(i) => &i.common,
            ItemBody::Script(i) => &i.common,
            ItemBody::Command(i) => &i.common,
            ItemBody::FileChange(i) => &i.common,
            ItemBody::Plan(i) => &i.common,
            ItemBody::Subagent(i) => &i.common,
            ItemBody::Web(i) => &i.common,
            ItemBody::Review(i) => &i.common,
            ItemBody::Compaction(i) => &i.common,
            ItemBody::Hook(i) => &i.common,
            ItemBody::Handoff(i) => &i.common,
            ItemBody::Error(i) => &i.common,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<ContextEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    pub status: UserStatus,
    /// Set by a driver when the item echoes a runtime prompt (Appendix E4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageId>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    Queued,
    Sent,
    Handled,
    Uncertain,
}

/// An `assistant` item; `text` is Markdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub text: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub tool: String,
    pub title: String,
    pub input: Value,
    pub status: ToolStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub caller: Caller,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Paused,
    Ok,
    Error,
    Cancelled,
}

/// The status of `script`, `subagent`, `command`, `file_change` and `web` items (Appendix E8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Ok,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    Model,
    Script,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScriptItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub language: String,
    /// Absent when the harness doesn't expose it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub status: RunStatus,
    pub background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub command: String,
    pub cwd: String,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub changes: Vec<FileChange>,
    pub status: RunStatus,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub op: FileOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileOp {
    Add,
    Modify,
    Delete,
    Move,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<PlanEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub text: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    Active,
    Done,
}

/// A subagent. Drivers report `key` (30.3); the runtime fills `session`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubagentItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub target: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompactionItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub trigger: CompactionTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Manual,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub event: String,
    pub name: String,
    pub outcome: HookOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookOutcome {
    Ok,
    Blocked,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HandoffItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub from: BindingId,
    pub to: BindingId,
    pub via: BindingVia,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorItem {
    #[serde(flatten)]
    pub common: ItemCommon,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
