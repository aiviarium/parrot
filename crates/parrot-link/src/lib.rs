//! Links between Parrot runtimes (spec 24, Appendix E9): carriers, Noise sessions, framing and
//! pairing.
//!
//! The runtime mounts a [`noise::Listener`] on its link endpoint (`link.sock` beside `sock`, or a
//! TCP listener) and hands it every accepted byte stream; the listener yields an authenticated
//! [`noise::LinkConnection`] with the peer's runtime id. To reach a peer, the runtime opens a
//! carrier ([`carrier::open`]) and runs [`noise::Dialer`] over it.

pub mod base32;
pub mod carrier;
pub mod endpoint;
pub mod error;
pub mod frame;
pub mod keys;
pub mod noise;
pub mod pairing;

pub use error::LinkError;
pub use keys::{PublicKey, StaticKey, runtime_id};
