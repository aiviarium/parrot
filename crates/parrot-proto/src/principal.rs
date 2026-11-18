//! Principals, scopes and isolation (spec 5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `con_…`, `ses_…` or `lnk_…`.
pub type Principal = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    Prompt,
    Answer,
    Configure,
    Manage,
    Delegate,
    Workspace,
    Destructive,
    Functions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    None,
    Sandbox,
    User,
    Container,
}

/// A client token for WebSocket connections (spec 3.4, Appendix E8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    pub id: crate::ids::TokenId,
    pub name: String,
    pub scopes: Vec<Scope>,
    /// Limits the token to these sessions' subtrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<crate::ids::SessionId>>,
    /// Patterns of the functions the token may provide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provides: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<crate::ids::Timestamp>,
    pub created: crate::ids::Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<crate::ids::Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenCreateParams {
    pub name: String,
    pub scopes: Vec<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<crate::ids::SessionId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provides: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<crate::ids::Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenCreateResult {
    pub token: Token,
    /// Shown once.
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenListResult {
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenRevokeParams {
    pub token: crate::ids::TokenId,
}
