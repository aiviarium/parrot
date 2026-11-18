//! A fake `opencode serve` for tests: the driver starts it exactly as it starts the real CLI.

fn main() -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(parrot_driver_opencode::fake::run())
}
