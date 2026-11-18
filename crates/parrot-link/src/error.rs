//! Errors of the link layer. Every one maps to `-32040` (link failed) with a `reason`.

use std::io;

/// The JSON-RPC code of a failed link (spec 37).
pub const LINK_FAILED: i64 = -32040;

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("noise: {0}")]
    Noise(#[from] snow::Error),
    /// The peer's static key isn't the pinned one, or isn't pinned at all.
    #[error("the peer's key {got} is not the one pinned for this link")]
    Key { got: String },
    /// A pairing code that is unknown, used, expired or malformed.
    #[error("pairing failed: {0}")]
    Pairing(String),
    /// The peer broke the link framing or handshake.
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("message of {0} bytes is over the limit")]
    TooLarge(usize),
    #[error("the handshake timed out")]
    Timeout,
    #[error("carrier: {0}")]
    Carrier(String),
}

impl LinkError {
    /// `data.reason` of the `-32040` error: `key` for a key that doesn't match (spec 24.3).
    pub fn reason(&self) -> &'static str {
        match self {
            LinkError::Key { .. } => "key",
            LinkError::Pairing(_) => "pairing",
            LinkError::Carrier(_) => "carrier",
            LinkError::Timeout => "timeout",
            LinkError::Io(_) => "io",
            LinkError::Noise(_) | LinkError::Protocol(_) | LinkError::TooLarge(_) => "protocol",
        }
    }

    /// The `data` of the `-32040` error.
    pub fn data(&self) -> serde_json::Value {
        serde_json::json!({ "reason": self.reason(), "detail": self.to_string() })
    }
}
