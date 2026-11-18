//! `parrot acp`: serves a Parrot runtime session as an ACP agent on standard input and output.

#[derive(clap::Args)]
pub struct Args {}

pub async fn run(_args: Args) -> Result<(), String> {
    Err("parrot acp is not implemented".into())
}
