//! See spec/protocol.md for the rules this module implements.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

/// Sessions by id.
#[derive(Debug, Default)]
pub struct Sessions {
    pub by_id: std::collections::BTreeMap<String, SessionRecord>,
}

/// What the core keeps per session; extend as needed.
#[derive(Debug, Clone, Default)]
pub struct SessionRecord {
    pub id: String,
    pub parent: Option<String>,
    pub binding: Option<String>,
    pub agent: String,
}

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}"))))
}
