//! See spec/protocol.md for the rules this module implements.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

/// This module's part of the core state.
#[derive(Debug, Default)]
pub struct Actions;

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}"))))
}

/// A driver answered a call this module made with `Cx::call_driver`.
pub fn on_reply(_cx: &mut Cx, _call: u64, _result: Result<Value, RpcError>) {}

/// A driver update of this area.
pub fn on_update(_cx: &mut Cx, _update: &parrot_proto::driver::DriverUpdate) {}
