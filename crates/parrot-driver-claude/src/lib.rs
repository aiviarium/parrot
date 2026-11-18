//! The Claude Code driver over stream-json, with a fake Claude for tests.

pub mod driver;
pub mod fake;
pub mod mapping;
pub mod wire;

/// Serves the Claude Code driver on stdin and stdout until stdin closes (`parrot driver claude`).
pub async fn serve_stdio() -> std::io::Result<()> {
    driver::serve(
        driver::Config::from_env(),
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}
