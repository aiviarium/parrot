//! Ids and other shared scalars (spec 2). Clients never parse ids, so they are plain strings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type SessionId = String;
pub type BindingId = String;
pub type MessageId = String;
pub type ItemId = String;
pub type RequestId = String;
pub type TurnId = String;
pub type QueueEntryId = String;
pub type WorkspaceId = String;
pub type EnvId = String;
pub type ConnectionId = String;
pub type RuntimeId = String;
pub type TaskId = String;
pub type SubscriptionId = String;
pub type LinkId = String;
pub type TokenId = String;
pub type AgentId = String;
/// An opaque paging or replay cursor (Appendix E2).
pub type Cursor = String;
/// RFC 3339 time in UTC.
pub type Timestamp = String;
/// Feature name to feature options; absent means unsupported.
pub type Features = Map<String, Value>;
/// `_meta` on any object (spec 38).
pub type Meta = Map<String, Value>;

/// The `{}` result of methods that return nothing; unknown fields are kept.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Empty {
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
