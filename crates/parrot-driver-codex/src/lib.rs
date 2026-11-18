//! The Codex driver: the Parrot driver protocol (spec 30) over the Codex app-server, with a fake
//! Codex for tests.

pub mod data;
pub mod driver;
pub mod fake;
pub mod frame;
pub mod input;
pub mod mapping;
pub mod wire;

/// Serves the Codex driver on stdin and stdout until stdin closes (`parrot driver codex`).
pub async fn serve_stdio() -> std::io::Result<()> {
    driver::serve(
        driver::Config::from_env(),
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}
