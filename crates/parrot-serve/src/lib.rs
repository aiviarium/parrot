//! Transports and dispatch for the Parrot runtime: local socket, named pipe, WebSocket, stdio.

pub mod dispatch;
pub mod executor;
pub mod socket;
pub mod stdio;
pub mod tokens;
pub mod websocket;
