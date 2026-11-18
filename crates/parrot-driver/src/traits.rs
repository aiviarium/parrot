//! The interface every driver implements, built in or out of process (spec 30).
//!
//! Methods and updates are the JSON shapes of spec 30.1 and 30.2. Built-in drivers implement
//! `Driver`; `host` adapts a process driver speaking JSON-RPC on stdio to the same trait.

use parrot_proto::rpc::RpcError;
use serde_json::Value;
use tokio::sync::mpsc;

/// An `update` notification from the driver: `{ binding, subagent?, update }` (spec 30.2).
pub type DriverUpdate = Value;

pub trait Driver: Send {
    /// Call a driver method such as `binding/start` or `binding/prompt` with its params.
    fn call(
        &mut self,
        method: &str,
        params: Value,
    ) -> impl std::future::Future<Output = Result<Value, RpcError>> + Send;
}

/// What a driver is given when it starts: where to send updates.
pub struct DriverContext {
    pub updates: mpsc::UnboundedSender<DriverUpdate>,
}
