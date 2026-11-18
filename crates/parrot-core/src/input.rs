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
        /// Function name patterns the client may provide (E8); `None` allows any.
        provides: Option<Vec<String>>,
    },
    /// A linked runtime, after the link handshake, with the grant and offer this runtime gave it.
    Link {
        link: String,
        scopes: Vec<String>,
        offer: LinkOffer,
    },
}

/// What a runtime shows a linked peer (spec 24.1): its environment, and which of its sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkOffer {
    /// Whether the peer sees this environment and its agents, and may create sessions in it.
    pub env: bool,
    pub sessions: OfferedSessions,
}

/// Which sessions a link's peer sees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OfferedSessions {
    /// None at all.
    #[default]
    None,
    /// Only sessions created over the link or moved here over it, and their descendants.
    Linked,
    /// Every session.
    All,
}

impl LinkOffer {
    /// Reads a link record's `offer`. Anything missing or unknown offers nothing.
    pub fn from_json(offer: &Value) -> LinkOffer {
        LinkOffer {
            env: offer.get("env").and_then(Value::as_bool).unwrap_or(false),
            sessions: match offer.get("sessions").and_then(Value::as_str) {
                Some("all") => OfferedSessions::All,
                Some("linked") => OfferedSessions::Linked,
                _ => OfferedSessions::None,
            },
        }
    }

    /// Everything: the environment and every session.
    pub fn all() -> LinkOffer {
        LinkOffer {
            env: true,
            sessions: OfferedSessions::All,
        }
    }
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
    /// A driver's answer to an `Effect::AgentCall`.
    AgentReply {
        agent: String,
        call: u64,
        result: Result<Value, RpcError>,
    },
    /// The answer to an `Effect::Workspace`.
    WorkspaceReply {
        call: u64,
        result: Result<Value, RpcError>,
    },
    /// What the store held at startup, fed in before any connection (spec 34).
    Restore(Restore),
}

/// The stored state a runtime starts from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Restore {
    /// The highest stored `seq`; the sequence continues after it.
    pub head: u64,
    /// Stored projections as `(kind, id, value)`, in the shapes the core persisted them.
    pub projections: Vec<(String, String, Value)>,
    /// The newest stored updates, oldest first, so replay reaches across the restart.
    pub recent: Vec<Stored>,
}

/// One stored update.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub seq: u64,
    pub session: Option<String>,
    pub at_ms: u64,
    pub update: Value,
}
