//! A fake T3 MCP server over HTTP for tests: it prints the address it took and serves `/mcp`.

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(0);
    parrot_driver_t3::fake::run_http(port)
}
