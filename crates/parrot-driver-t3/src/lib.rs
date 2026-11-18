//! The T3 Code driver (spec Appendix B): Parrot driving T3 through its orchestration MCP tools.

pub mod driver;
pub mod fake;
pub mod mcp;

/// Serves the driver protocol on stdin and stdout (`parrot driver t3`).
pub async fn serve_stdio() -> std::io::Result<()> {
    let (read, write) = (tokio::io::stdin(), tokio::io::stdout());
    driver::serve(driver::Config::from_env(), read, write).await
}
