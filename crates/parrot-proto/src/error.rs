//! Error codes (spec 37, Appendix E) and their conversion to JSON-RPC errors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rpc::RpcError;

pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const UNSUPPORTED: i64 = -32001;
pub const NOT_FOUND: i64 = -32002;
pub const FORBIDDEN: i64 = -32003;
pub const WRONG_STATE: i64 = -32004;
pub const NOT_INITIALIZED: i64 = -32010;
pub const NO_SHARED_VERSION: i64 = -32011;
pub const REPLAY_UNAVAILABLE: i64 = -32012;
pub const ALREADY_ANSWERED: i64 = -32013;
pub const HARNESS_FAILED: i64 = -32020;
pub const ENVIRONMENT_OFFLINE: i64 = -32030;
pub const BUDGET_EXHAUSTED: i64 = -32032;
pub const LINK_FAILED: i64 = -32040;
/// The session moved to another runtime; the old owner keeps a redirect.
pub const MOVED: i64 = -32050;
/// The binding cannot provide the isolation the ceiling requires.
pub const ISOLATION: i64 = -32051;

/// `data` of `-32001`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UnsupportedData {
    pub feature: String,
}

/// `data` of `-32003`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ForbiddenData {
    pub reason: String,
}

/// `data` of `-32004`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WrongStateData {
    pub state: String,
}

/// `data` of `-32011`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoSharedVersionData {
    pub supported: Vec<u32>,
}

/// `data` of `-32020` and `-32040`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DetailData {
    pub detail: String,
}

/// `data` of `-32050`: the runtime that owns the session now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MovedData {
    pub runtime: String,
}

/// `data` of `-32051`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IsolationData {
    pub required: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<String>,
}

/// A protocol error, with the `data` each code carries.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParrotError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("method not found: {0}")]
    MethodNotFound(String),
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("unsupported: {feature}")]
    Unsupported { feature: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("forbidden: {reason}")]
    Forbidden { reason: String },
    #[error("wrong state: {state}")]
    WrongState { state: String },
    #[error("not initialized")]
    NotInitialized,
    #[error("no shared version")]
    NoSharedVersion { supported: Vec<u32> },
    #[error("replay unavailable")]
    ReplayUnavailable,
    #[error("already answered")]
    AlreadyAnswered,
    #[error("harness failed: {detail}")]
    HarnessFailed { detail: String },
    #[error("environment offline: {0}")]
    EnvironmentOffline(String),
    #[error("budget exhausted")]
    BudgetExhausted,
    #[error("link failed: {detail}")]
    LinkFailed { detail: String },
    #[error("moved to {runtime}")]
    Moved { runtime: String },
    #[error("isolation {required} unavailable")]
    Isolation {
        required: String,
        available: Option<String>,
    },
}

impl ParrotError {
    pub fn code(&self) -> i64 {
        use ParrotError::*;
        match self {
            InvalidRequest(_) => INVALID_REQUEST,
            MethodNotFound(_) => METHOD_NOT_FOUND,
            InvalidParams(_) => INVALID_PARAMS,
            Unsupported { .. } => UNSUPPORTED,
            NotFound(_) => NOT_FOUND,
            Forbidden { .. } => FORBIDDEN,
            WrongState { .. } => WRONG_STATE,
            NotInitialized => NOT_INITIALIZED,
            NoSharedVersion { .. } => NO_SHARED_VERSION,
            ReplayUnavailable => REPLAY_UNAVAILABLE,
            AlreadyAnswered => ALREADY_ANSWERED,
            HarnessFailed { .. } => HARNESS_FAILED,
            EnvironmentOffline(_) => ENVIRONMENT_OFFLINE,
            BudgetExhausted => BUDGET_EXHAUSTED,
            LinkFailed { .. } => LINK_FAILED,
            Moved { .. } => MOVED,
            Isolation { .. } => ISOLATION,
        }
    }

    fn data(&self) -> Option<Value> {
        use ParrotError::*;
        let v = match self {
            Unsupported { feature } => serde_json::to_value(UnsupportedData {
                feature: feature.clone(),
            }),
            Forbidden { reason } => serde_json::to_value(ForbiddenData {
                reason: reason.clone(),
            }),
            WrongState { state } => serde_json::to_value(WrongStateData { state: state.clone() }),
            NoSharedVersion { supported } => serde_json::to_value(NoSharedVersionData {
                supported: supported.clone(),
            }),
            HarnessFailed { detail } | LinkFailed { detail } => serde_json::to_value(DetailData {
                detail: detail.clone(),
            }),
            Moved { runtime } => serde_json::to_value(MovedData {
                runtime: runtime.clone(),
            }),
            Isolation { required, available } => serde_json::to_value(IsolationData {
                required: required.clone(),
                available: available.clone(),
            }),
            _ => return None,
        };
        Some(v.expect("error data serializes"))
    }
}

impl From<ParrotError> for RpcError {
    fn from(e: ParrotError) -> Self {
        let mut r = RpcError::new(e.code(), e.to_string());
        r.data = e.data();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn carries_data() {
        let r: RpcError = ParrotError::Moved {
            runtime: "rt_example".into(),
        }
        .into();
        assert_eq!(r.code, -32050);
        assert_eq!(r.data, Some(json!({"runtime": "rt_example"})));
        let r: RpcError = ParrotError::NotInitialized.into();
        assert_eq!((r.code, r.data), (-32010, None));
        let r: RpcError = ParrotError::NoSharedVersion { supported: vec![1] }.into();
        assert_eq!(r.data, Some(json!({"supported": [1]})));
    }
}
