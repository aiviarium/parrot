//! Agents (spec 6).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::action::Action;
use crate::ids::{AgentId, EnvId, Features, Meta};
use crate::option::{ConfigOption, PermissionLevel};
use crate::principal::Isolation;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: AgentId,
    /// Absent when a driver reports it; the runtime sets it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub env: EnvId,
    pub name: String,
    pub driver: AgentDriver,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessInfo>,
    pub installed: bool,
    pub auth: AuthStatus,
    pub usable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub controls: Vec<Control>,
    pub features: Features,
    pub levels: Vec<PermissionLevel>,
    pub isolation: Vec<Isolation>,
    pub options: Vec<ConfigOption>,
    pub actions: Vec<Action>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDriver {
    pub name: String,
    pub version: String,
    pub kind: DriverKind,
    /// The driver data version.
    pub data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Builtin,
    Process,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInfo {
    pub version: String,
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    SignedIn,
    SignedOut,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Native,
    Tui,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentListResult {
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRefreshParams {
    pub agent: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRefreshResult {
    pub agent: Agent,
}
