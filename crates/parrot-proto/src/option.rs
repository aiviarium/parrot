//! Options and permission levels (spec 15). The spec's `Option` is `ConfigOption` here, so it
//! doesn't shadow `std::option::Option`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{AgentId, Meta, SessionId};
use crate::open::Open;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOption {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Open<OptionRole>>,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema: Value,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    pub applies: Applies,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<DependsOn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OptionRole {
    Model,
    Effort,
    Mode,
    Sandbox,
    Speed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    Start,
    Now,
    NextTurn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DependsOn {
    pub option: String,
    pub schemas: Map<String, Value>,
}

/// What an agent can do without asking anyone, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    Read,
    Write,
    Sandboxed,
    Host,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OptionSetParams {
    pub session: SessionId,
    pub values: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OptionSetResult {
    pub options: Vec<ConfigOption>,
    pub reset: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<PermissionLevel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OptionCompleteParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    pub option: String,
    pub prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OptionCompleteResult {
    pub values: Vec<CompletionValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompletionValue {
    #[serde(rename = "const")]
    pub value: Value,
    pub title: String,
}

/// `option/list` (Appendix E8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OptionListParams {
    pub session: SessionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OptionListResult {
    pub options: Vec<ConfigOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<PermissionLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
}
