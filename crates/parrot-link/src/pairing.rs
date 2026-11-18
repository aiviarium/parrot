//! Pairing (spec 24.3): invites holding the inviter's key and a one-time secret, and the rule
//! that settles a simultaneous dial (E9).
//!
//! The secret is the pre-shared key of `Noise_XXpsk3`, so only the holder of the code can finish
//! the pairing handshake, and the code's key lets the joining side check it reached the inviter.

use crate::keys::PublicKey;
use crate::{LinkError, base32};
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

/// How long an invite is valid unless `link/invite` says otherwise.
pub const DEFAULT_TTL_MS: u64 = 10 * 60 * 1000;

const CODE_PREFIX: &str = "plk1_";
const CODE_VERSION: u8 = 1;
const INVITE_LEN: usize = 16;
const SECRET_LEN: usize = 32;
const KEY_LEN: usize = 32;
const MAX_ADDRESSES: usize = 8;
const MAX_CODE: usize = 1024;

/// The one-time secret of an invite. Its `Debug` hides it, and it is zeroed when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub(crate) [u8; SECRET_LEN]);

impl Secret {
    pub fn random() -> Secret {
        Secret(rand::random())
    }

    pub fn from_bytes(bytes: [u8; SECRET_LEN]) -> Secret {
        Secret(bytes)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for b in self.0.iter_mut() {
            // SAFETY: a valid, aligned pointer to a byte of this struct.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

/// What a pairing code carries. The inviter's runtime id is derived from `key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteCode {
    pub key: PublicKey,
    pub invite: String,
    pub secret: Secret,
    /// Where the inviter listens, if it does, such as `host:port` for `tcp`.
    pub addresses: Vec<String>,
}

impl InviteCode {
    /// The inviter's runtime id.
    pub fn runtime(&self) -> String {
        self.key.runtime_id()
    }

    /// The code as text: `plk1_` and Crockford base32 of the version, key, invite, secret and
    /// newline-separated addresses. It holds the secret, so it is shown once and never logged.
    pub fn encode(&self) -> String {
        let invite = invite_bytes(&self.invite).expect("invite ids come from new_invite_id");
        let mut bytes = vec![CODE_VERSION];
        bytes.extend_from_slice(&self.key.0);
        bytes.extend_from_slice(&invite);
        bytes.extend_from_slice(&self.secret.0);
        bytes.extend_from_slice(self.addresses.join("\n").as_bytes());
        let text = format!("{CODE_PREFIX}{}", base32::encode(&bytes));
        bytes.fill(0);
        text
    }

    pub fn parse(text: &str) -> Result<InviteCode, LinkError> {
        let bad = |what: &str| LinkError::Pairing(format!("not a pairing code: {what}"));
        let text = text.trim();
        if text.len() > MAX_CODE {
            return Err(bad("too long"));
        }
        let body = text
            .strip_prefix(CODE_PREFIX)
            .ok_or_else(|| bad("wrong prefix"))?;
        let mut bytes = base32::decode(body).ok_or_else(|| bad("not base32"))?;
        let fixed = 1 + KEY_LEN + INVITE_LEN + SECRET_LEN;
        let result = (|| {
            if bytes.len() < fixed || bytes[0] != CODE_VERSION {
                return Err(bad("unknown version or truncated"));
            }
            let key = PublicKey::from_slice(&bytes[1..1 + KEY_LEN]).expect("length checked");
            let invite = invite_id(&bytes[1 + KEY_LEN..1 + KEY_LEN + INVITE_LEN]);
            let mut secret = [0u8; SECRET_LEN];
            secret.copy_from_slice(&bytes[1 + KEY_LEN + INVITE_LEN..fixed]);
            let secret = Secret(secret);
            let rest = std::str::from_utf8(&bytes[fixed..]).map_err(|_| bad("addresses"))?;
            let addresses: Vec<String> = if rest.is_empty() {
                Vec::new()
            } else {
                rest.split('\n').map(str::to_owned).collect()
            };
            if addresses.len() > MAX_ADDRESSES
                || addresses
                    .iter()
                    .any(|a| a.is_empty() || a.chars().any(char::is_control))
            {
                return Err(bad("addresses"));
            }
            Ok(InviteCode {
                key,
                invite,
                secret,
                addresses,
            })
        })();
        bytes.fill(0);
        result
    }
}

/// A new random invite id, `inv_` and 26 Crockford base32 characters.
pub fn new_invite_id() -> String {
    invite_id(&rand::random::<[u8; INVITE_LEN]>())
}

fn invite_id(bytes: &[u8]) -> String {
    format!("inv_{}", base32::encode(bytes))
}

fn invite_bytes(id: &str) -> Option<[u8; INVITE_LEN]> {
    let bytes = base32::decode(id.strip_prefix("inv_")?)?;
    bytes.try_into().ok()
}

/// The pending invites of a runtime, as the responder of a pairing handshake sees them.
pub trait Invites: Send + Sync {
    /// The secret of an invite that is pending and not expired at `now_ms`.
    fn secret(&self, invite: &str, now_ms: u64) -> Option<Secret>;
    /// Marks the invite used once its handshake succeeded. False if it was used, revoked or
    /// expired in the meantime, which fails the pairing.
    fn claim(&self, invite: &str, now_ms: u64) -> bool;
}

struct Pending {
    secret: Secret,
    expires_ms: u64,
}

/// Invites in memory. An invite is removed when it is claimed, revoked or found expired.
#[derive(Default)]
pub struct InviteBook {
    pending: Mutex<HashMap<String, Pending>>,
}

impl InviteBook {
    pub fn new() -> InviteBook {
        InviteBook::default()
    }

    /// A new invite for this runtime's `key`, valid for `ttl_ms` from `now_ms`.
    pub fn create(&self, key: PublicKey, addresses: Vec<String>, ttl_ms: u64, now_ms: u64) -> InviteCode {
        let code = InviteCode {
            key,
            invite: new_invite_id(),
            secret: Secret::random(),
            addresses,
        };
        let pending = Pending {
            secret: code.secret.clone(),
            expires_ms: now_ms.saturating_add(ttl_ms),
        };
        self.lock().insert(code.invite.clone(), pending);
        code
    }

    pub fn revoke(&self, invite: &str) -> bool {
        self.lock().remove(invite).is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.pending.lock().expect("invites lock")
    }
}

impl Invites for InviteBook {
    fn secret(&self, invite: &str, now_ms: u64) -> Option<Secret> {
        let mut pending = self.lock();
        pending.retain(|_, p| p.expires_ms > now_ms);
        pending.get(invite).map(|p| p.secret.clone())
    }

    fn claim(&self, invite: &str, now_ms: u64) -> bool {
        matches!(self.lock().remove(invite), Some(p) if p.expires_ms > now_ms)
    }
}

/// Which side opened a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// This runtime dialled the peer.
    Dialled,
    /// The peer dialled this runtime.
    Accepted,
}

/// The side whose dialled connection survives when both runtimes dial at once: the connection
/// dialled by the lower runtime id is kept (E9).
pub fn winning_side(local: &str, peer: &str) -> Side {
    if local < peer {
        Side::Dialled
    } else {
        Side::Accepted
    }
}

/// Whether a new connection to a peer replaces the current one. In the same direction the new
/// one is a reconnect and replaces it; in opposite directions the tie-break decides, so both
/// runtimes keep the same connection.
pub fn keep_new(local: &str, peer: &str, current: Side, new: Side) -> bool {
    current == new || new == winning_side(local, peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StaticKey;

    #[test]
    fn code_round_trip() {
        let key = StaticKey::from_private([3; 32]).public();
        let book = InviteBook::new();
        let code = book.create(key, vec!["203.0.113.7:7790".into()], DEFAULT_TTL_MS, 1_000);
        let text = code.encode();
        assert!(text.starts_with("plk1_"));
        let parsed = InviteCode::parse(&text).unwrap();
        assert_eq!(parsed, code);
        assert_eq!(parsed.runtime(), key.runtime_id());
        assert_eq!(InviteCode::parse(&text.to_lowercase()).unwrap(), code);
    }

    #[test]
    fn bad_codes_are_refused() {
        assert!(InviteCode::parse("plk1_").is_err());
        assert!(InviteCode::parse("xyz_ABC").is_err());
        assert!(InviteCode::parse(&format!("plk1_{}", "0".repeat(2000))).is_err());
        let mut bytes = vec![2u8];
        bytes.extend_from_slice(&[0; 80]);
        assert!(InviteCode::parse(&format!("plk1_{}", base32::encode(&bytes))).is_err());
    }

    #[test]
    fn invites_expire_and_are_used_once() {
        let key = StaticKey::from_private([3; 32]).public();
        let book = InviteBook::new();
        let code = book.create(key, vec![], 1_000, 10_000);
        assert_eq!(book.secret(&code.invite, 10_500), Some(code.secret.clone()));
        assert!(book.claim(&code.invite, 10_600));
        assert!(!book.claim(&code.invite, 10_700), "a code works once");
        assert_eq!(book.secret(&code.invite, 10_700), None);

        let late = book.create(key, vec![], 1_000, 10_000);
        assert_eq!(book.secret(&late.invite, 11_000), None, "expired");
        assert!(!book.claim(&late.invite, 11_000));

        let revoked = book.create(key, vec![], 1_000, 10_000);
        assert!(book.revoke(&revoked.invite));
        assert_eq!(book.secret(&revoked.invite, 10_001), None);
    }

    #[test]
    fn simultaneous_dial_keeps_the_lower_ids_connection() {
        let (low, high) = ("rt_0AAAAAAAAAAAAAAAAAAAAAAAAA", "rt_ZAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(winning_side(low, high), Side::Dialled);
        assert_eq!(winning_side(high, low), Side::Accepted);
        // Each side holds its own dial and receives the other's; both keep the same one: the
        // one `low` dialled.
        assert!(!keep_new(low, high, Side::Dialled, Side::Accepted));
        assert!(keep_new(high, low, Side::Dialled, Side::Accepted));
        assert!(keep_new(low, high, Side::Accepted, Side::Dialled));
        assert!(!keep_new(high, low, Side::Accepted, Side::Dialled));
        // A reconnect in the same direction replaces a stale connection.
        assert!(keep_new(low, high, Side::Accepted, Side::Accepted));
    }
}
