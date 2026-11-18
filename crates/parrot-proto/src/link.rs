//! Links between runtimes (spec 24, E1, E9): link records, carriers, pairing, and the methods
//! one runtime calls on a linked one to move sessions and workspaces.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{LinkId, RuntimeId, SessionId, Timestamp, WorkspaceId};
use crate::principal::Scope;
use crate::workspace::Workspace;

/// What a runtime shows a linked peer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkOffer {
    /// Whether the peer sees this environment and its agents, and may create sessions in it.
    pub env: bool,
    pub sessions: OfferedSessions,
}

/// Which sessions a link's peer sees. `linked`: only sessions created over the link or moved
/// here over it, with their descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfferedSessions {
    #[default]
    None,
    Linked,
    All,
}

/// How a runtime reaches a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CarrierRef {
    /// `ssh`, `command`, `tcp`, or a carrier egg's name.
    pub kind: String,
    /// Validated against the carrier's target schema.
    pub target: Map<String, Value>,
    /// Tried in order when this one fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Vec<CarrierRef>>,
}

/// Whether the carrier's credentials reach more of the peer than its link endpoint (spec 24.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Containment {
    Restricted,
    Shell,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Connected,
    Connecting,
    Down,
    Unpaired,
}

/// The other runtime of a link, pinned at pairing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkPeer {
    pub runtime: RuntimeId,
    /// The peer's X25519 public key, in hex.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub id: LinkId,
    pub name: String,
    pub peer: LinkPeer,
    /// Absent if this runtime only accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<CarrierRef>,
    /// What the peer may do here.
    pub grant: Vec<Scope>,
    pub offer: LinkOffer,
    pub containment: Containment,
    pub status: LinkStatus,
    /// The carrier instance of the current connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct LinkListParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkListResult {
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct CarrierListParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CarrierInfo {
    pub kind: String,
    pub title: String,
    /// The JSON Schema of the carrier's target.
    pub target: Value,
    pub listens: bool,
    pub containment: Containment,
    pub max_message: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CarrierListResult {
    pub carriers: Vec<CarrierInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkInviteParams {
    pub name: String,
    pub grant: Vec<Scope>,
    pub offer: LinkOffer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
    /// Set when the peer's SSH key was written to `authorized_keys` restricted to
    /// `parrot connect --link`: the peer then sees its link as `restricted` (spec 24.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_key: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkInviteResult {
    /// The one-time pairing code. It is a secret: shown once, never logged.
    pub code: String,
    pub link: Link,
}

/// The grant and offer `link/add` asks the peer for when it pairs over the carrier (spec 24.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkAsk {
    pub grant: Vec<Scope>,
    pub offer: LinkOffer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkAddParams {
    pub name: String,
    pub carrier: CarrierRef,
    /// A pairing code from the peer's `link/invite`; without it, the runtime pairs over the
    /// carrier with `ask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub grant: Vec<Scope>,
    pub offer: LinkOffer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<LinkAsk>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkResult {
    pub link: Link,
}

pub type LinkAddResult = LinkResult;
pub type LinkUpdateResult = LinkResult;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkUpdateParams {
    pub link: LinkId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<CarrierRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<Vec<Scope>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<LinkOffer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkRemoveParams {
    pub link: LinkId,
}

/// One chunk of a transfer between runtimes, at most 1 MiB (spec 24.2, E9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutParams {
    pub transfer: String,
    pub index: u64,
    pub data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<bool>,
}

/// The first phase of a move (E1): the owner offers a session to the target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOfferParams {
    pub session: SessionId,
    /// The session as the owner stores it.
    pub record: Value,
    /// The session's workspace; `transfer` names its blob when it travels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    /// The transfer that carries the session's items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_keys: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Vec<Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionOfferResult {
    /// The binding the target started idle.
    pub binding: Option<Value>,
}

/// The second phase of a move (E9): keyed and idempotent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionCommitParams {
    pub session: SessionId,
    pub key: String,
}

pub type SessionCommitResult = SessionOfferResult;

/// What a peer's `workspace/transfer` sends: unpack the transfer as a new workspace here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReceiveParams {
    pub workspace: WorkspaceId,
    pub transfer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceReceiveResult {
    pub workspace: Workspace,
}
