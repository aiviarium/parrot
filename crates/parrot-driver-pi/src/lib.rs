//! The pi driver: the Parrot driver protocol (spec 30) over pi RPC.

pub mod data;
pub mod driver;
pub mod fake;
pub mod history;
pub mod mapping;
pub mod wire;

/// Serves the driver protocol on stdin and stdout (`parrot driver pi`).
pub async fn serve_stdio() -> std::io::Result<()> {
    driver::serve(
        driver::Config::from_env(),
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}
