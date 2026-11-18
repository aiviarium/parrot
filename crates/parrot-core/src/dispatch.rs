//! Routes inputs to modules. Each area adds its methods to `call`; keep one line per prefix.

use crate::{Cx, Effect, Input};
use parrot_proto::rpc::RpcError;

pub(crate) fn input(cx: &mut Cx, input: Input) {
    match input {
        Input::Call {
            conn,
            id,
            method,
            params,
        } => {
            let result = call(cx, &conn, &method, params);
            if let Some(id) = id {
                cx.emit(Effect::Reply { conn, id, result });
            }
        }
        Input::Connected { .. }
        | Input::Disconnected { .. }
        | Input::ClientReply { .. }
        | Input::DriverUpdate { .. }
        | Input::DriverReply { .. }
        | Input::DriverExited { .. }
        | Input::Tick { .. } => {}
    }
}

/// A method's result. Handlers that answer later return `Ok(Pending)` in a later sprint; for now
/// every method answers at once.
fn call(
    _cx: &mut Cx,
    _conn: &str,
    method: &str,
    _params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    #[allow(clippy::match_single_binding)]
    match method.split('/').next().unwrap_or("") {
        _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
    }
}
