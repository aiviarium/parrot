//! Stands in for `codex app-server` by replaying a scenario; see `parrot_driver_codex::fake`.

use std::path::PathBuf;

use parrot_driver_codex::fake;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The version of the recorded traces the fake replays.
    if args.iter().any(|a| a == "--version") {
        println!("codex-cli 0.167.1");
        return;
    }
    let Some(path) = fake::scenario_arg(&args).or_else(|| std::env::var(fake::SCENARIO_ENV).ok()) else {
        eprintln!("fake-codex: give --scenario <path> or set {}", fake::SCENARIO_ENV);
        std::process::exit(64);
    };
    let steps = match fake::load_scenario(&PathBuf::from(&path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fake-codex: {path}: {e}");
            std::process::exit(64);
        }
    };
    let code = fake::run(&steps, std::io::stdin().lock(), std::io::stdout().lock());
    std::process::exit(code);
}
