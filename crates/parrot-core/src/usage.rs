//! Usage (spec 17.1). Budgets come later.

use std::collections::BTreeMap;

use crate::{Cx, Reply, directory};
use parrot_proto::driver::{DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::usage::{Cost, TokenUsage, Usage as UsageReport};
use serde_json::{Value, json};

/// Usage and budgets by session.
#[derive(Debug, Default)]
pub struct Usage {
    pub by_session: BTreeMap<String, SessionUsage>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionUsage {
    pub total: UsageReport,
    /// By turn driver key, for updates that name their turn.
    pub by_turn: BTreeMap<String, UsageReport>,
}

/// Handle a client method of this area.
pub fn call(_cx: &mut Cx, _conn: &str, _id: Option<&RequestId>, method: &str, _params: Value) -> Reply {
    Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}"))))
}

fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

/// Add `delta` into `into`: tokens and cost add up, context and rate limits are the latest.
fn accumulate(into: &mut UsageReport, delta: &UsageReport) {
    if let Some(t) = &delta.tokens {
        let cur = into.tokens.get_or_insert_with(TokenUsage::default);
        cur.input += t.input;
        cur.output += t.output;
        cur.cache_read = add_opt(cur.cache_read, t.cache_read);
        cur.cache_write = add_opt(cur.cache_write, t.cache_write);
        cur.reasoning = add_opt(cur.reasoning, t.reasoning);
    }
    if let Some(c) = &delta.cost {
        match &mut into.cost {
            Some(cur) if cur.currency == c.currency => cur.amount += c.amount,
            Some(_) => {}
            None => {
                into.cost = Some(Cost {
                    amount: c.amount,
                    currency: c.currency.clone(),
                })
            }
        }
    }
    if delta.context.is_some() {
        into.context = delta.context.clone();
    }
    if delta.rate_limits.is_some() {
        into.rate_limits = delta.rate_limits.clone();
    }
    if delta.counted_in.is_some() {
        into.counted_in = delta.counted_in.clone();
    }
}

/// A usage update from a driver.
pub fn on_update(cx: &mut Cx, update: &DriverUpdate) {
    let Open::Known(DriverUpdateKind::Usage { usage }) = &update.update else {
        return;
    };
    let Some(session) = directory::session_of(cx.state, &update.binding) else {
        return;
    };
    let entry = cx.state.usage.by_session.entry(session.clone()).or_default();
    accumulate(&mut entry.total, usage);
    if let Some(turn) = update.extra.get("turn").and_then(Value::as_str) {
        accumulate(entry.by_turn.entry(turn.to_string()).or_default(), usage);
    }
    let total = entry.total.clone();
    cx.persist(json!({ "op": "put", "kind": "usage", "id": session, "value": total }));
    cx.publish(Some(session), json!({ "type": "usage", "usage": total }));
}

/// Put back a session's usage read from the store.
pub fn restore(state: &mut crate::State, session: &str, value: &Value) {
    if let Ok(total) = serde_json::from_value(value.clone()) {
        state
            .usage
            .by_session
            .entry(session.to_string())
            .or_default()
            .total = total;
    }
}
