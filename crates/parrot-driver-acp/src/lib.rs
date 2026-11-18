//! The ACP driver (spec Appendix A): the Parrot driver protocol over any ACP v1 agent, and the
//! pieces `parrot acp` uses to serve a Parrot session as an ACP agent.

pub mod data;
pub mod driver;
pub mod fake;
pub mod mapping;
pub mod wire;

/// Serves the driver protocol on stdin and stdout (`parrot driver acp`).
pub async fn serve_stdio() -> std::io::Result<()> {
    let (read, write) = (tokio::io::stdin(), tokio::io::stdout());
    driver::serve(driver::Config::from_env(), read, write).await
}
