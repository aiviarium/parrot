//! See spec/protocol.md for the rules this module implements.

use crate::Cx;
use parrot_proto::rpc::RpcError;
use serde_json::Value;

/// Bindings by id.
#[derive(Debug, Default)]
pub struct Bindings {
    pub by_id: std::collections::BTreeMap<String, BindingRecord>,
}

/// What the core keeps per binding; extend as needed.
#[derive(Debug, Clone, Default)]
pub struct BindingRecord {
    pub id: String,
    pub session: String,
    pub agent: String,
}

/// A driver answered a call this module made with `Cx::call_driver`.
pub fn on_reply(_cx: &mut Cx, _call: u64, _result: Result<Value, RpcError>) {}

/// A driver update other than requests, options, actions and usage.
pub fn on_update(_cx: &mut Cx, _update: &parrot_proto::driver::DriverUpdate) {}

/// The driver process for a binding exited.
pub fn exited(_cx: &mut Cx, _binding: &str, _detail: &str) {}
