//! The Codex driver: the Parrot driver protocol (spec 30) over the Codex app-server.

/// Serves the driver protocol on stdin and stdout (`parrot driver codex`).
pub async fn serve_stdio() -> std::io::Result<()> {
    Err(std::io::Error::other("the codex driver is not written yet"))
}
