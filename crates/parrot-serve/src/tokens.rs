//! Checking WebSocket client tokens (spec 3.4).

use parrot_core::Peer;
use std::collections::HashMap;

/// Turns the secret from a `parrot.token.<token>` subprotocol into the connection's peer.
pub trait TokenCheck: Send + Sync + 'static {
    /// The peer for a valid, unexpired token, or `None` to refuse the upgrade.
    fn check(&self, token: &str) -> Option<Peer>;
}

/// A fixed table of tokens, for tests and embedding.
#[derive(Debug, Default, Clone)]
pub struct StaticTokens(pub HashMap<String, Peer>);

impl TokenCheck for StaticTokens {
    fn check(&self, token: &str) -> Option<Peer> {
        self.0.get(token).cloned()
    }
}
