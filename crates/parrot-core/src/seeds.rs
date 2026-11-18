//! Seeds (spec 9.3, 9.4, 23, E6): fork, handoff, rebind, rewind, bundles.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32001, format!("unsupported: {method}"))
        .with_data(json!({ "feature": "profile.seed" }))))
}

/// A driver, workspace or client reply to a call this module made.
pub fn on_reply(_cx: &mut Cx, _call: u64, _result: Result<Value, RpcError>) {}
