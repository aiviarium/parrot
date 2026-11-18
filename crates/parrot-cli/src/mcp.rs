//! `parrot mcp`: the MCP server a harness starts for its binding (spec 26).

#[derive(clap::Args)]
pub struct Args {}

pub async fn run(_args: Args) -> Result<(), String> {
    Err("parrot mcp is not implemented".into())
}
