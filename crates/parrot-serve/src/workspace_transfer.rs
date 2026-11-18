//! Workspace operations that cross runtimes: packing a workspace for `workspace/transfer` and
//! unpacking one that arrived over a link (spec 8, 24, E1).

use parrot_proto::rpc::RpcError;
use serde_json::{Value, json};

/// Run a transfer operation; `None` when `op` is not one.
pub fn run(op: &str, _params: &Value) -> Option<Result<Value, RpcError>> {
    if !op.starts_with("transfer.") {
        return None;
    }
    Some(Err(RpcError::new(
        -32001,
        format!("unsupported workspace operation: {op}"),
    )
    .with_data(json!({ "feature": "profile.link" }))))
}
