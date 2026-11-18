//! Functions (spec 25–29, E5): the catalog, calls between clients, harnesses and providers,
//! and the session functions.

use crate::{Cx, Reply};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32001, format!("unsupported: {method}"))
        .with_data(json!({ "feature": "profile.tree" }))))
}

/// A driver, workspace or client reply to a call this module made.
pub fn on_reply(_cx: &mut Cx, _call: u64, _result: Result<Value, RpcError>) {}

/// A client's answer to an `Effect::ClientCall`.
pub fn on_client_reply(_cx: &mut Cx, _conn: &str, _call: u64, _result: Result<Value, RpcError>) {}
