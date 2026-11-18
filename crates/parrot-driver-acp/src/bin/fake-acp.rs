//! Stands in for an ACP agent in tests by replaying a script; see `parrot_driver_acp::fake`.

use std::path::PathBuf;

use parrot_driver_acp::fake;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = fake::scenario_arg(&args).or_else(|| std::env::var(fake::SCENARIO_ENV).ok());
    let script = match path {
        Some(path) => match fake::load_script(&PathBuf::from(&path)) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("fake-acp: {path}: {e}");
                std::process::exit(64);
            }
        },
        None => None,
    };
    let code = fake::run(script, std::io::stdin().lock(), std::io::stdout());
    std::process::exit(code);
}
