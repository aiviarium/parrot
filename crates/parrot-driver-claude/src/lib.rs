//! The Claude Code driver over stream-json, with a fake Claude for tests.

pub mod driver;
pub mod fake;
pub mod mapping;
pub mod wire;

/// Serves the Claude driver on standard input and output (`parrot driver claude`).
pub async fn serve_stdio() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the claude driver is not served on stdio yet",
    ))
}
