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
