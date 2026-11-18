//! Seeds and bundles (spec 23, Appendix D). `SeedRequest` and the bundle's items are raw JSON.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{SessionId, TurnId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionBundleParams {
    pub session: SessionId,
    /// A turn id; the latest when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<TurnId>,
    /// `none` or `source`; `source` asks the source to summarize itself and needs `delegate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionBundleResult {
    /// The `ContextBundle` (23.2).
    pub bundle: Value,
    /// The bundle as Markdown (Appendix D).
    pub rendered: String,
}
