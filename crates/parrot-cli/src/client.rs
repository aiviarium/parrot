//! The client commands: `parrot sessions`, `parrot prompt` and the rest, over the local socket.

#[derive(clap::Subcommand)]
pub enum Command {}

pub async fn run(command: Command) -> Result<(), String> {
    match command {}
}
