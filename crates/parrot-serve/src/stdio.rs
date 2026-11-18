//! One connection on standard input and output, as the owner.

use crate::dispatch::Runtime;
use crate::framing::serve_lines;
use parrot_core::Peer;
use std::io;

/// Serves stdin and stdout as one owner connection until stdin ends.
pub async fn serve_stdio(rt: Runtime) -> io::Result<()> {
    serve_lines(rt, Peer::Owner, tokio::io::stdin(), tokio::io::stdout()).await
}
