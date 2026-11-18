//! Tasks (spec 19, 20.1, Appendix E7, E8). `TaskRequest` and `Task` are carried as raw JSON in
//! sessions; these are the method params and results.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{Cursor, SessionId};

/// `TaskRequest` (19).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskRequest {
    /// The child's first prompt.
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A JSON Schema the output must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// `finish` or `idle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_on: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskAssignParams {
    pub session: SessionId,
    pub task: TaskRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// The result of `task/assign`: the session's new `Task`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskAssignResult {
    pub task: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskFinishParams {
    pub session: SessionId,
    /// Structured when the task has a `result` schema, else `{ text }`.
    pub output: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskFailParams {
    pub session: SessionId,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskWaitParams {
    pub sessions: Vec<SessionId>,
    /// A cursor from an earlier call; the first call leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Cursor>,
    /// Seconds, capped by the harness's tool-call timeout (20.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskChange {
    pub session: SessionId,
    /// The session's `Task` as it is now.
    pub task: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskWaitResult {
    pub changes: Vec<TaskChange>,
    pub cursor: Cursor,
    pub timed_out: bool,
}
