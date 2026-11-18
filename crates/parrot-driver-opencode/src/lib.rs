//! The opencode driver: the Parrot driver protocol (spec 30) over the opencode HTTP server.

/// Serves the driver protocol on stdin and stdout (`parrot driver opencode`).
pub async fn serve_stdio() -> std::io::Result<()> {
    Err(std::io::Error::other("the opencode driver is not written yet"))
}
