//! What the runtime feeds into the core. The serve crate turns sockets, driver processes and timers
//! into these; the core never does I/O itself.

use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::Value;

/// A connection id, issued by the transport (`con_…`).
pub type ConnId = String;
/// A binding id (`bnd_…`); driver traffic is addressed by binding.
pub type BindingId = String;

/// Who is on the other end of a connection, as the transport established it (spec 5.1, 3.4).
#[derive(Debug, Clone, PartialEq)]
pub enum Peer {
    /// The owner on the local socket or stdio: every scope.
    Owner,
    /// A WebSocket client with a token's scopes and session filter.
    Token {
        token: String,
        scopes: Vec<String>,
        sessions: Option<Vec<String>>,
    },
    /// A linked runtime, after the link handshake.
    Link { link: String, scopes: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Connected {
        conn: ConnId,
        peer: Peer,
    },
    Disconnected {
        conn: ConnId,
    },
    /// A client request. Notifications from clients arrive with `id: None`.
    Call {
        conn: ConnId,
        id: Option<RequestId>,
        method: String,
        params: Value,
    },
    /// A client's answer to a call the runtime made to it (`function/call`).
    ClientReply {
        conn: ConnId,
        call: u64,
        result: Result<Value, RpcError>,
    },
    /// An `update` notification from a driver (spec 30.2), params as sent.
    DriverUpdate {
        binding: BindingId,
        update: Value,
    },
    /// A driver's answer to an `Effect::DriverCall`.
    DriverReply {
        binding: BindingId,
        call: u64,
        result: Result<Value, RpcError>,
    },
    /// The driver process for a binding is gone.
    DriverExited {
        binding: BindingId,
        detail: String,
    },
    /// Time passed; the core checks expiries and timers.
    Tick {
        now_ms: u64,
    },
}
