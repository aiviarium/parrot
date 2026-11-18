//! State and turns (spec 11).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{BindingId, MessageId, Meta, Timestamp, TurnId};
use crate::open::Open;
use crate::usage::Usage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Idle,
    Running,
    RequiresAction,
    Stopped,
    Lost,
    Unrecognised,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: TurnId,
    /// Omitted when a driver reports the turn.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub binding: BindingId,
    pub status: TurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Open<TurnStop>>,
    pub continuable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<MessageId>,
    pub started: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TurnError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Running,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnStop {
    End,
    MaxTokens,
    TurnLimit,
    Refusal,
    Cancelled,
    Budget,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
