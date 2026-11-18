//! `parrot link`: pairing and link records on the local runtime (spec 24).

#[derive(clap::Args)]
pub struct Args {}

pub async fn run(_args: Args) -> Result<(), String> {
    Err("parrot link is not implemented".into())
}
