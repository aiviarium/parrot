//! Requests (spec 14).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{Cursor, ItemId, Meta, RequestId, SessionId, Timestamp, TurnId};
use crate::open::Open;
use crate::option::PermissionLevel;
use crate::session::Ref;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: RequestId,
    /// Omitted when a driver reports the request.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: SessionId,
    /// Sessions from the root to this one; omitted by drivers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<SessionId>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    pub kind: Open<RequestKind>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Open<Subject>>,
    pub level: PermissionLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<Choice>>,
    /// A request with several questions: one entry per question, answered by id in
    /// `Answer.answers`. Absent for a single question, which uses `choices`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    pub blocking: bool,
    pub answerable: Answerable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Answer>,
    pub notify: Notify,
    pub status: RequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// A principal, or `harness`, `fallback`, `ceiling`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Approval,
    Question,
    Plan,
    Input,
    Auth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<ChoiceIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ChoiceScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amends: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceIntent {
    Allow,
    Deny,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceScope {
    Once,
    Session,
    Always,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Subject {
    Tool {
        tool: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editable: Option<bool>,
    },
    Command {
        command: String,
        cwd: String,
    },
    Files {
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
    },
    Network {
        host: String,
    },
    Plan {
        text: String,
    },
    Function {
        function: String,
        input: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// The answer to each of a request's `questions`, by question id: a choice id, a label, a
    /// list of them, or text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One question of a request that asks several (spec 14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// The id `Answer.answers` uses.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The question itself.
    pub question: String,
    /// Whether more than one option may be picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    /// The options to pick from; empty for free text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Choice>,
}

/// Who can still answer once things go away. `process` was removed (Appendix E8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Answerable {
    Connection,
    Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    None,
    Normal,
    Urgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Open,
    Answered,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestAnswerParams {
    pub request: RequestId,
    pub answer: Answer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RequestStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestListResult {
    pub requests: Vec<Request>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}
