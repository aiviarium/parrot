//! See spec/protocol.md for the rules this module implements.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

/// Connections and what each may do.
#[derive(Debug, Default)]
pub struct Principals;

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}"))))
}

/// A connection opened.
pub fn connected(_cx: &mut Cx, _conn: &str, _peer: crate::Peer) {}

/// A connection closed.
pub fn disconnected(_cx: &mut Cx, _conn: &str) {}
