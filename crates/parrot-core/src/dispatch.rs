//! Routes every input to the module that owns it.

use crate::{
    CallOwner, Cx, Effect, Input, Reply, actions, agents, bindings, functions, links, options, principals,
    queue, requests, seeds, sessions, subscriptions, tasks, usage,
};
use parrot_proto::driver::{DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::rpc::RpcError;

pub(crate) fn input(cx: &mut Cx, input: Input) {
    match input {
        Input::Connected { conn, peer } => principals::connected(cx, &conn, peer),
        Input::Disconnected { conn } => {
            requests::disconnected(cx, &conn);
            tasks::disconnected(cx, &conn);
            subscriptions::disconnected(cx, &conn);
            functions::disconnected(cx, &conn);
            links::disconnected(cx, &conn);
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
        Input::ClientReply { conn, call, result } => functions::on_client_reply(cx, &conn, call, result),
        Input::DriverUpdate { binding, update } => driver_update(cx, &binding, update),
        Input::DriverReply {
            binding,
            call,
            result,
        } => match cx.state.pending_calls.remove(&call) {
            Some(CallOwner::Bindings) => bindings::on_reply(cx, call, result),
            Some(CallOwner::Queue) => queue::on_reply(cx, call, result),
            Some(CallOwner::Requests) => requests::on_reply(cx, call, result),
            Some(CallOwner::Options) => options::on_reply(cx, call, result),
            Some(CallOwner::Actions) => actions::on_reply(cx, call, result),
            Some(CallOwner::Sessions) => sessions::on_reply(cx, call, result),
            Some(owner) => reply_to(cx, owner, call, result),
            // The backend sends `binding/start` itself, under a call id the core never issued.
            None => bindings::start_reply(cx, &binding, result),
        },
        Input::AgentReply { agent, call, result } => agents::on_agent_reply(cx, &agent, call, result),
        Input::WorkspaceReply { call, result } => {
            match cx.state.pending_calls.remove(&call) {
                // The tree's workspace calls go out under `Tasks`.
                Some(CallOwner::Tasks) if crate::tree::on_reply(cx, call, result.clone()) => {}
                Some(owner) => reply_to(cx, owner, call, result),
                None => {}
            }
        }
        Input::Restore(restore) => crate::restore::restore(cx, restore),
        Input::DriverExited { binding, detail } => bindings::exited(cx, &binding, &detail),
        Input::Tick { .. } => {
            requests::tick(cx);
            queue::tick(cx);
            tasks::tick(cx);
            seeds::tick(cx);
            crate::budgets::tick(cx);
            crate::ceilings::tick(cx);
            links::tick(cx);
        }
    }
    subscriptions::flush(cx);
}

/// Route a reply to the module that made the call.
fn reply_to(cx: &mut Cx, owner: CallOwner, call: u64, result: Result<serde_json::Value, RpcError>) {
    match owner {
        CallOwner::Bindings => bindings::on_reply(cx, call, result),
        CallOwner::Queue => queue::on_reply(cx, call, result),
        CallOwner::Requests => requests::on_reply(cx, call, result),
        CallOwner::Options => options::on_reply(cx, call, result),
        CallOwner::Actions => actions::on_reply(cx, call, result),
        CallOwner::Sessions => sessions::on_reply(cx, call, result),
        CallOwner::Tasks => tasks::on_reply(cx, call, result),
        CallOwner::Seeds => seeds::on_reply(cx, call, result),
        CallOwner::Links => links::on_reply(cx, call, result),
        CallOwner::Functions => functions::on_reply(cx, call, result),
    }
}

pub(crate) fn call(
    cx: &mut Cx,
    conn: &str,
    id: Option<&serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> Reply {
    if let Some(reply) = links::intercept(cx, conn, id, method, &params) {
        return reply;
    }
    let area = method.split('/').next().unwrap_or("");
    match area {
        "initialize" | "token" => principals::call(cx, conn, id, method, params),
        "session" | "env" | "workspace" => sessions::call(cx, conn, id, method, params),
        "agent" => agents::call(cx, conn, id, method, params),
        "task" => tasks::call(cx, conn, id, method, params),
        "link" | "carrier" | "blob" => links::call(cx, conn, id, method, params),
        "function" => functions::call(cx, conn, id, method, params),
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
