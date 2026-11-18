//! `parrot probe`: runs a probe scenario against a real CLI and records its frames and transcript. Not run in CI.

#[derive(clap::Args)]
pub struct Args {}

pub async fn run(_args: Args) -> Result<(), String> {
    Err("parrot probe is not implemented".into())
}
