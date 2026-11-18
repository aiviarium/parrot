//! Functions (spec 25, 27.1, Appendix E5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::action::Effect;
use crate::ids::{Cursor, EnvId, SessionId};
use crate::option::PermissionLevel;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionExample {
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

/// A function a provider offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionSpec {
    /// Namespaced, dot-separated; `parrot.` is reserved for this spec (27.1).
    pub name: String,
    pub description: String,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    pub effects: Vec<Effect>,
    /// The lowest permission level that allows a call without asking (E5).
    pub level: PermissionLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<Vec<FunctionExample>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
    /// Listed directly to harnesses (26).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A function with its provider: `runtime`, a connection id or a link id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionInfo {
    #[serde(flatten)]
    pub spec: FunctionSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionContent {
    Text { text: String },
    Image { data: String, mime: String },
    Resource { uri: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionError {
    pub code: String,
    pub message: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The result of `function/call`, from the runtime to a client and from a provider to the
/// runtime.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct FunctionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<FunctionContent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FunctionError>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct FunctionListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionListResult {
    pub functions: Vec<FunctionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionSearchParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionDescribeParams {
    pub names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionDescribeResult {
    pub functions: Vec<FunctionInfo>,
    /// The functions as TypeScript declarations.
    pub declarations: String,
}

/// `function/call`, from a client to the runtime and from the runtime to the providing client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionCallParams {
    pub function: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvId>,
    pub input: Value,
    /// The session the call is for; the runtime sets it on calls it forwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Seconds to wait for an approval, capped by the harness's tool-call timeout (27.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionProvideParams {
    pub functions: Vec<FunctionSpec>,
}
