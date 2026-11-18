//! How the runtime finds an agent's driver: the built-in drivers today, installed driver eggs
//! (the `parrot` Hatchery target) next.

use std::path::PathBuf;

/// How to run a driver: the program, its arguments and extra environment.
#[derive(Debug, Clone)]
pub struct DriverCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The driver for `agent` when no command was configured for it.
pub fn resolve(agent: &str) -> Result<DriverCommand, String> {
    match agent {
        "claude" | "codex" | "acp" | "pi" | "opencode" | "t3" => {
            let program = std::env::current_exe().map_err(|e| format!("cannot find parrot: {e}"))?;
            Ok(DriverCommand {
                program,
                args: vec!["driver".into(), agent.into()],
                env: Vec::new(),
            })
        }
        _ => Err(format!("no driver for agent {agent}")),
    }
}
