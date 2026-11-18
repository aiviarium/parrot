//! The T3 Code driver (spec Appendix B): Parrot driving T3 Code through its `/mcp` tools.

/// Serves the driver protocol on stdin and stdout (`parrot driver t3`).
pub async fn serve_stdio() -> std::io::Result<()> {
    Err(std::io::Error::other("the t3 driver is not written yet"))
}
