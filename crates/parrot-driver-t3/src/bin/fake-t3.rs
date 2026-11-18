//! A fake T3 MCP server for tests: the orchestration tools over stdio, without T3.

fn main() -> std::io::Result<()> {
    parrot_driver_t3::fake::run_stdio()
}
