//! Transports and dispatch for the Parrot runtime: local socket, named pipe, WebSocket, stdio.
//!
//! `dispatch::Runtime` owns the core in one task. Each transport turns a connection into
//! `Input`s for it, and `executor` turns the core's effects back into messages and backend calls.

pub mod dispatch;
pub mod executor;
pub mod framing;
pub mod socket;
pub mod stdio;
pub mod tokens;
pub mod websocket;

pub use dispatch::{Runtime, new_conn_id};
pub use executor::{Backend, NoopBackend, RecordingBackend};
pub use tokens::TokenCheck;
