//! Subscriptions, updates and reading (spec 10).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::action::Action;
use crate::agent::Agent;
use crate::env::Environment;
use crate::ids::{ConnectionId, Cursor, ItemId, Meta, RuntimeId, SessionId, SubscriptionId, Timestamp};
use crate::item::{Item, ItemPatch};
use crate::open::Open;
use crate::option::{ConfigOption, PermissionLevel};
use crate::principal::Principal;
use crate::prompt::QueueState;
use crate::request::Request;
use crate::session::{Background, Session, SessionSummary};
use crate::state::{State, Turn};
use crate::usage::Usage;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Target {
    Session {
        session: SessionId,
    },
    /// A session and its descendants in one stream.
    Tree {
        tree: SessionId,
    },
    Index {
        index: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeParams {
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_from: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResult {
    pub subscription: SubscriptionId,
    pub head: Cursor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<SessionSnapshot>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<IndexSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexSnapshot {
    pub sessions: Vec<SessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
    pub agents: Vec<Agent>,
    pub envs: Vec<Environment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session: Session,
    /// The newest items, oldest first.
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Cursor>,
    pub requests: Vec<Request>,
    pub queue: QueueState,
    pub options: Vec<ConfigOption>,
    pub actions: Vec<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub subscribers: Vec<ConnectionInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub id: ConnectionId,
    pub name: String,
    pub principal: Principal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeParams {
    pub subscription: SubscriptionId,
}

/// Params of the `update` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateParams {
    pub subscription: SubscriptionId,
    /// The update's place in the runtime's sequence; an opaque cursor (Appendix E2).
    pub seq: Cursor,
    /// The runtime that emitted it (Appendix E2).
    pub runtime: RuntimeId,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// Unknown update types are kept raw and ignored.
    pub update: Open<Update>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Update {
    State {
        state: State,
        background: Vec<Background>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Session {
        session: Box<Session>,
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
    Queue {
        queue: QueueState,
    },
    Options {
        options: Vec<ConfigOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<PermissionLevel>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<bool>,
    },
    Actions {
        actions: Vec<Action>,
    },
    Usage {
        usage: Usage,
    },
    Notice {
        level: NoticeLevel,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    Summary {
        session: SessionSummary,
    },
    Agent {
        agent: Box<Agent>,
    },
    Env {
        env: Environment,
    },
    Workspace {
        workspace: Workspace,
    },
    Deleted {
        kind: DeletedKind,
        id: String,
    },
    /// The pinned functions changed, so an MCP listing built from them is stale (spec 26).
    Functions {
        pinned: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeletedKind {
    Session,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadDirection {
    Back,
    Forward,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<ReadDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResult {
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemGetParams {
    pub session: SessionId,
    pub item: ItemId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemGetResult {
    pub item: Item,
}
