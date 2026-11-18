//! The core's state, one part per module so modules can be written side by side.

use crate::{actions, bindings, items, options, principals, queue, requests, sessions, subscriptions, usage};

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
    /// The runtime-wide update sequence (spec 10.1).
    pub seq: u64,
    /// The next id for calls the core makes to drivers and clients.
    pub next_call: u64,
}
