//! Links and carriers (spec 24, Appendix E1, E9). Moves carry logs and queues as raw JSON.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{LinkId, RuntimeId, SessionId, Timestamp};
use crate::principal::Scope;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkPeer {
    pub runtime: RuntimeId,
    /// The peer's public key, pinned at pairing.
    pub key: String,
}

/// How a runtime reaches its peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CarrierRef {
    /// `ssh`, `command`, `tcp`, or a carrier egg's name.
    pub kind: String,
    /// Validated against the carrier's target schema.
    pub target: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Vec<CarrierRef>>,
}

/// What a link shows its peer: the environment, and `none`, `linked` or `all` sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkOffer {
    pub env: bool,
    pub sessions: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Link {
    pub id: LinkId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<LinkPeer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<CarrierRef>,
    pub grant: Vec<Scope>,
    pub offer: LinkOffer,
    /// `restricted`, `shell` or `unknown` (24.4).
    pub containment: String,
    /// `connected`, `connecting`, `down` or `unpaired`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkListResult {
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CarrierInfo {
    pub kind: String,
    pub title: String,
    pub target: Value,
    pub listens: bool,
    pub containment: String,
    pub max_message: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CarrierListResult {
    pub carriers: Vec<CarrierInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkInviteParams {
    pub name: String,
    pub grant: Vec<Scope>,
    pub offer: LinkOffer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkInviteResult {
    /// A pairing code, valid once.
    pub code: String,
    pub link: Link,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkAddParams {
    pub name: String,
    pub carrier: CarrierRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub grant: Vec<Scope>,
    pub offer: LinkOffer,
    /// Pairing over the carrier: the grant and offer to ask the peer for (24.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkResult {
    pub link: Link,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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

/// One chunk of a transfer over a link, at most 1 MiB (E9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BlobPutParams {
    pub transfer: String,
    pub index: u64,
    /// Base64.
    pub data: String,
    pub last: bool,
}

/// A move's first phase, from the owner (E1). The log travels as `blob/put` chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOfferParams {
    pub session: SessionId,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A move's second phase: the target owns the session on receipt (E9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionCommitParams {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}
