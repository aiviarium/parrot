//! Stands in for `claude` in stream-json mode by replaying a scenario; see `parrot_driver_claude::fake`.

use std::path::PathBuf;

use parrot_driver_claude::fake;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = fake::scenario_arg(&args).or_else(|| std::env::var(fake::SCENARIO_ENV).ok()) else {
        eprintln!(
            "fake-claude: give --scenario <path> or set {}",
            fake::SCENARIO_ENV
        );
        std::process::exit(64);
    };
    let steps = match fake::load_scenario(&PathBuf::from(&path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fake-claude: {path}: {e}");
            std::process::exit(64);
        }
    };
    let code = fake::run(&steps, std::io::stdin().lock(), std::io::stdout().lock());
    std::process::exit(code);
}
