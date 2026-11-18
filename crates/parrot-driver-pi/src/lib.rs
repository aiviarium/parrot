//! The pi driver: the Parrot driver protocol (spec 30) over pi RPC.

/// Serves the driver protocol on stdin and stdout (`parrot driver pi`).
pub async fn serve_stdio() -> std::io::Result<()> {
    Err(std::io::Error::other("the pi driver is not written yet"))
}
