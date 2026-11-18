//! The core's state, one part per module so modules can be written side by side.

use crate::{
    actions, bindings, ceilings, items, options, principals, queue, requests, seeds, sessions, subscriptions,
    tasks, tree, usage,
};

#[derive(Debug, Default)]
pub struct State {
    pub principals: principals::Principals,
    pub sessions: sessions::Sessions,
    pub bindings: bindings::Bindings,
    pub queue: queue::Queues,
    pub items: items::Items,
    pub subscriptions: subscriptions::Subscriptions,
    pub requests: requests::Requests,
    pub options: options::Options,
    pub actions: actions::Actions,
    pub usage: usage::Usage,
    pub tree: tree::Tree,
    pub tasks: tasks::Tasks,
    pub seeds: seeds::Seeds,
    pub ceilings: ceilings::Ceilings,
    /// The runtime-wide update sequence (spec 10.1).
    pub seq: u64,
    /// The next id for calls the core makes to drivers and clients.
    pub next_call: u64,
    /// Which module waits for each outstanding driver call.
    pub pending_calls: std::collections::BTreeMap<u64, CallOwner>,
    /// Updates published during the current input, flushed by `subscriptions::flush`.
    pub outbox: Vec<(Option<String>, serde_json::Value)>,
}

/// The module a driver reply goes back to.
#[derive(Debug, Clone, PartialEq)]
pub enum CallOwner {
    Bindings,
    Queue,
    Requests,
    Options,
    Actions,
    Tasks,
    Seeds,
    Links,
    Functions,
    Sessions,
}
