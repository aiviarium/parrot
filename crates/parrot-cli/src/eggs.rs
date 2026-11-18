//! `parrot eggs`: install, list and check driver eggs (the `parrot` Hatchery target).

#[derive(clap::Args)]
pub struct Args {}

pub async fn run(_args: Args) -> Result<(), String> {
    Err("parrot eggs is not implemented".into())
}
