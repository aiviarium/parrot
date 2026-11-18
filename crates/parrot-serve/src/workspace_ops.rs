//! Runs the core's `Effect::Workspace` operations with `parrot-workspace`, off the actor thread.

use parrot_proto::rpc::RpcError;
use serde_json::{Value, json};

/// Run one operation; blocking, so the backend calls it from a blocking task.
pub fn run(op: &str, params: Value) -> Result<Value, RpcError> {
    if let Some(result) = crate::workspace_transfer::run(op, &params) {
        return result;
    }
    Err(
        RpcError::new(-32001, format!("unsupported workspace operation: {op}"))
            .with_data(json!({ "feature": "profile.tree" })),
    )
}
