//! `parrot conform`: runs the golden fixtures against a driver or a runtime (spec 38).

#[derive(clap::Args)]
pub struct Args {
    /// The driver command to test, such as `parrot driver claude`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

pub fn run(_args: Args) -> i32 {
    eprintln!("parrot conform: not implemented");
    2
}
