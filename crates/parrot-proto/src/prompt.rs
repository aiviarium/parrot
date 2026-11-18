//! Prompts and the queue (spec 13).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{MessageId, Meta, QueueEntryId, SessionId};
use crate::item::Origin;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptParams {
    pub session: SessionId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<ContextEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub name: String,
    /// A media type.
    #[serde(rename = "type")]
    pub media_type: String,
    /// Base64 content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default]
    Auto,
    Queue,
    Steer,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptResult {
    pub message: MessageId,
    pub disposition: PromptDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<QueueEntryId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptDisposition {
    Started,
    Steered,
    Queued,
    Handled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCancelParams {
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCancelResult {
    pub interrupted: bool,
    pub queued: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueState {
    pub paused: bool,
    pub entries: Vec<QueueEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub id: QueueEntryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageId>,
    pub text: String,
    pub origin: Origin,
    pub kind: QueueEntryKind,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Appendix E4 adds `action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueEntryKind {
    Prompt,
    Action,
    Wake,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueRemoveParams {
    pub session: SessionId,
    pub id: QueueEntryId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueEditParams {
    pub session: SessionId,
    pub id: QueueEntryId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueMoveParams {
    pub session: SessionId,
    pub id: QueueEntryId,
    pub index: u32,
}

/// Params of `queue/clear`, `queue/resume` and `queue/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSessionParams {
    pub session: SessionId,
}

pub type QueueClearParams = QueueSessionParams;
pub type QueueResumeParams = QueueSessionParams;
pub type QueueListParams = QueueSessionParams;

/// `queue/list` (Appendix E8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueListResult {
    pub queue: QueueState,
}
