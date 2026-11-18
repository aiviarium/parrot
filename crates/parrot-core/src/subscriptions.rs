//! See spec/protocol.md for the rules this module implements.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

/// Subscriptions by connection.
#[derive(Debug, Default)]
pub struct Subscriptions;

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}"))))
}

/// Assign `seq` to the updates in `cx.state.outbox`, persist them and send them to subscribers.
pub fn flush(cx: &mut Cx) {
    cx.state.outbox.clear();
}

/// A connection closed.
pub fn disconnected(_cx: &mut Cx, _conn: &str) {}
