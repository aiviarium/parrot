//! What the core asks the runtime to do. The serve crate executes these in order.

use crate::input::{BindingId, ConnId};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Answer a client request.
    Reply {
        conn: ConnId,
        id: RequestId,
        result: Result<Value, RpcError>,
    },
    /// Send a notification (`update`, …) to a client.
    Notify {
        conn: ConnId,
        method: String,
        params: Value,
    },
    /// Call a function a client provides (`function/call`); the answer comes back as
    /// `Input::ClientReply` with the same `call`.
    ClientCall {
        conn: ConnId,
        call: u64,
        method: String,
        params: Value,
    },
    /// Start the driver for a binding, then send it `binding/start` with `start`.
    StartBinding {
        binding: BindingId,
        agent: String,
        start: Value,
    },
    /// Call a driver method (spec 30.1); the answer comes back as `Input::DriverReply`.
    DriverCall {
        binding: BindingId,
        call: u64,
        method: String,
        params: Value,
    },
    /// Run `agent`'s driver without a binding, call `method` on it and stop it; the answer comes
    /// back as `Input::AgentReply` with the same `call` (spec 30.1 `agent/list`).
    AgentCall {
        agent: String,
        call: u64,
        method: String,
        params: Value,
    },
    /// Stop a binding's driver process after `binding/stop`.
    StopBinding { binding: BindingId },
    /// Append a record to the store (update log, intent log, projections).
    Persist { record: Value },
    /// Wake the core with `Input::Tick` at this time.
    TimerAt { at_ms: u64 },
    /// Close a connection.
    Close { conn: ConnId },
}
