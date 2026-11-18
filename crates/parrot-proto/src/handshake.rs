//! `initialize` (spec 4).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{ConnectionId, EnvId, Features, Meta, RuntimeId, SessionId};
use crate::principal::{Principal, Scope};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol: Vec<u32>,
    pub peer: PeerInfo,
    pub features: Features,
    /// `FunctionSpec`s this client offers (25.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provides: Option<Vec<Value>>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol: u32,
    pub peer: PeerInfo,
    pub runtime: RuntimeInfo,
    pub features: Features,
    pub connection: ConnectionGrant,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub kind: PeerKind,
    pub name: String,
    pub version: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PeerKind {
    Client,
    Runtime,
    Driver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub id: RuntimeId,
    pub env: EnvId,
}

/// The `connection` of `InitializeResult`: who the caller is and what it may do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionGrant {
    pub id: ConnectionId,
    pub principal: Principal,
    pub scopes: Vec<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<SessionId>>,
}
