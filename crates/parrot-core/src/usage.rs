//! See spec/protocol.md for the rules this module implements.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

/// Usage and budgets by session.
#[derive(Debug, Default)]
pub struct Usage;

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}"))))
}

/// A usage update from a driver.
pub fn on_update(_cx: &mut Cx, _update: &parrot_proto::driver::DriverUpdate) {}
