//! Routes every input to the module that owns it. This file is fixed; modules fill in their
//! functions.

use crate::{
    CallOwner, Cx, Effect, Input, Reply, actions, bindings, options, principals, queue, requests, sessions,
    subscriptions, usage,
};
use parrot_proto::driver::{DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::rpc::RpcError;

pub(crate) fn input(cx: &mut Cx, input: Input) {
    match input {
        Input::Connected { conn, peer } => principals::connected(cx, &conn, peer),
        Input::Disconnected { conn } => {
            requests::disconnected(cx, &conn);
            subscriptions::disconnected(cx, &conn);
            principals::disconnected(cx, &conn);
        }
        Input::Call {
            conn,
            id,
            method,
            params,
        } => {
            let reply = call(cx, &conn, id.as_ref(), &method, params);
            if let (Reply::Now(result), Some(id)) = (reply, id) {
                cx.emit(Effect::Reply { conn, id, result });
            }
        }
        Input::ClientReply { .. } => {}
        Input::DriverUpdate { binding, update } => driver_update(cx, &binding, update),
        Input::DriverReply { call, result, .. } => match cx.state.pending_calls.remove(&call) {
            Some(CallOwner::Bindings) => bindings::on_reply(cx, call, result),
            Some(CallOwner::Queue) => queue::on_reply(cx, call, result),
            Some(CallOwner::Requests) => requests::on_reply(cx, call, result),
            Some(CallOwner::Options) => options::on_reply(cx, call, result),
            Some(CallOwner::Actions) => actions::on_reply(cx, call, result),
            None => {}
        },
        Input::DriverExited { binding, detail } => bindings::exited(cx, &binding, &detail),
        Input::Tick { .. } => {
            requests::tick(cx);
            queue::tick(cx);
        }
    }
    subscriptions::flush(cx);
}

fn call(
    cx: &mut Cx,
    conn: &str,
    id: Option<&serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> Reply {
    let area = method.split('/').next().unwrap_or("");
    match area {
        "initialize" | "token" => principals::call(cx, conn, id, method, params),
        "session" | "agent" | "env" | "workspace" => sessions::call(cx, conn, id, method, params),
        "queue" => queue::call(cx, conn, id, method, params),
        "subscribe" | "unsubscribe" | "item" => subscriptions::call(cx, conn, id, method, params),
        "request" => requests::call(cx, conn, id, method, params),
        "option" => options::call(cx, conn, id, method, params),
        "action" => actions::call(cx, conn, id, method, params),
        "usage" => usage::call(cx, conn, id, method, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn driver_update(cx: &mut Cx, binding: &str, raw: serde_json::Value) {
    let Ok(mut update) = serde_json::from_value::<DriverUpdate>(raw) else {
        return;
    };
    // The binding a driver names can't be trusted over the one its process was started for.
    update.binding = binding.to_string();
    match &update.update {
        Open::Known(DriverUpdateKind::Request { .. }) => requests::on_update(cx, &update),
        Open::Known(DriverUpdateKind::Options { .. }) => options::on_update(cx, &update),
        Open::Known(DriverUpdateKind::Actions { .. }) => actions::on_update(cx, &update),
        Open::Known(DriverUpdateKind::Usage { .. }) => usage::on_update(cx, &update),
        _ => bindings::on_update(cx, &update),
    }
}
