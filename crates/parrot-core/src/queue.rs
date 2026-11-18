//! See spec/protocol.md for the rules this module implements.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

/// Queues by session.
#[derive(Debug, Default)]
pub struct Queues;

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}"))))
}

/// A driver answered a call this module made with `Cx::call_driver`.
pub fn on_reply(_cx: &mut Cx, _call: u64, _result: Result<Value, RpcError>) {}

/// Time passed.
pub fn tick(_cx: &mut Cx) {}
