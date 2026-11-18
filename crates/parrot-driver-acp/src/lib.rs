//! The ACP driver (spec Appendix A): the Parrot driver protocol over an ACP agent, and `export`, which serves a Parrot runtime session as an ACP agent.

/// Serves the driver protocol on stdin and stdout (`parrot driver acp`).
pub async fn serve_stdio() -> std::io::Result<()> {
    Err(std::io::Error::other("the acp driver is not written yet"))
}
