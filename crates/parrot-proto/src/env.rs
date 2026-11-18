//! Environments (spec 7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{EnvId, Features, LinkId, Meta, RuntimeId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub id: EnvId,
    pub runtime: RuntimeId,
    pub name: String,
    /// Absent for the local environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkId>,
    pub os: Os,
    pub arch: String,
    pub status: EnvStatus,
    pub roots: Vec<String>,
    pub features: Features,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnvStatus {
    Online,
    Offline,
}

pub type EnvListParams = crate::ids::Empty;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvListResult {
    pub envs: Vec<Environment>,
}
