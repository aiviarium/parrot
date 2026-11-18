//! How the runtime finds an agent's driver: from its installed driver egg (the `parrot`
//! Hatchery target). Nothing is compiled in, so with no egg installed there are no agents.
//!
//! | `driver.kdl` | The runtime runs |
//! | --- | --- |
//! | `driver "builtin" name="acp"` | `parrot driver acp` |
//! | `driver "builtin" name="tui"` | The TUI core in process, with the egg's `profile.kdl` (E4); see [`Drivers::tui_agent`] |
//! | `driver "command" { … }` | The program in the egg's folder for this OS and architecture |
//!
//! Every driver is started with `PARROT_DRIVER_DATA` set to the egg's `parrot` folder, and with
//! `PARROT_DRIVER_AGENT` and `PARROT_DRIVER_AGENT_NAME` set to the agent it serves.

use parrot_core::agents::AgentEgg;
use parrot_eggs::{DriverRun, LoadedEgg};
use parrot_proto::agent::{Control, DriverKind};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How to run a driver: the program, its arguments and extra environment.
#[derive(Debug, Clone)]
pub struct DriverCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The loaded driver eggs, by agent id.
#[derive(Debug, Clone, Default)]
pub struct Drivers {
    eggs: BTreeMap<String, LoadedEgg>,
}

impl Drivers {
    pub fn new(eggs: impl IntoIterator<Item = LoadedEgg>) -> Self {
        Self {
            eggs: eggs.into_iter().map(|e| (e.agent.clone(), e)).collect(),
        }
    }

    /// The eggs installed in the runtime in `state_dir`. Problems reading them are returned
    /// beside.
    pub fn load(state_dir: &std::path::Path) -> (Self, Vec<String>) {
        let (eggs, problems) = parrot_eggs::Eggs::new(state_dir).load();
        (Self::new(eggs), problems)
    }

    pub fn eggs(&self) -> impl Iterator<Item = &LoadedEgg> {
        self.eggs.values()
    }

    /// What the core needs to know of each agent.
    pub fn agents(&self) -> Vec<AgentEgg> {
        self.eggs.values().map(agent_egg).collect()
    }

    /// The driver for `agent`, or why it has none.
    pub fn resolve(&self, agent: &str) -> Result<DriverCommand, String> {
        let egg = self
            .eggs
            .get(agent)
            .ok_or_else(|| format!("no driver egg for agent {agent}"))?;
        let driver = egg
            .driver
            .as_ref()
            .map_err(|e| format!("cannot use the driver egg of {agent}: {e}"))?;
        let env = vec![
            (
                parrot_eggs::DATA_ENV.to_string(),
                egg.data_dir.to_string_lossy().into_owned(),
            ),
            (parrot_eggs::AGENT_ENV.to_string(), egg.agent.clone()),
            (parrot_eggs::AGENT_NAME_ENV.to_string(), egg.name.clone()),
        ];
        match &driver.driver {
            // The TUI core runs in the runtime (E4); see `tui_agent`.
            DriverRun::Builtin { name } if name == "tui" => {
                Err(format!("agent {agent} runs under TUI control in the runtime"))
            }
            DriverRun::Builtin { name } => Ok(DriverCommand {
                program: parrot_exe()?,
                args: vec!["driver".into(), name.clone()],
                env,
            }),
            DriverRun::Command { .. } => {
                let (run, args) = driver
                    .command_for(std::env::consts::OS, std::env::consts::ARCH)
                    .expect("a command driver")
                    .map_err(|e| format!("cannot use the driver egg of {agent}: {e}"))?;
                Ok(DriverCommand {
                    program: egg.data_dir.join(run),
                    args,
                    env,
                })
            }
        }
    }
}

impl Drivers {
    /// TUI control from the egg alone (spec 31, 32.2): an egg whose `controls` include `tui` and
    /// that ships `profile.kdl` drives its CLI through the terminal. The command is the
    /// `PARROT_<AGENT>_BIN` variable when set, else the egg's `cli` name. Claude Code writes its
    /// own transcript, named by `transcript` in its driver.kdl, which supplies the content. `PARROT_TUI_SIZE` (`<rows>x<cols>`) sets the
    /// terminal size.
    pub fn tui_agent(&self, agent: &str) -> Option<crate::config::TuiAgent> {
        let egg = self.eggs.get(agent)?;
        let driver = egg.driver.as_ref().ok()?;
        let is_core = matches!(&driver.driver, DriverRun::Builtin { name } if name == "tui");
        if !is_core && !driver.controls.contains(&Control::Tui) {
            return None;
        }
        let profile = egg.data_dir.join(PROFILE);
        if !profile.is_file() {
            return None;
        }
        // A data-only egg on the TUI core may give the whole command.
        let text = std::fs::read_to_string(egg.data_dir.join("driver.kdl")).unwrap_or_default();
        let var = format!("PARROT_{}_BIN", agent.to_uppercase().replace('-', "_"));
        let command = match std::env::var(&var).ok().filter(|p| !p.is_empty()) {
            Some(program) => vec![program],
            None => parrot_proto::driver_data::driver_command(&text)
                .or_else(|| driver.cli.as_ref().map(|c| vec![c.name.clone()]))?,
        };
        // The egg says where its transcript is and how the MCP server is passed.
        let transcript = driver.transcript.as_ref().map(|t| {
            let format = serde_json::to_value(t.format).ok();
            crate::config::TranscriptConfig {
                format: format
                    .and_then(|f| f.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                dir: t.dir.clone(),
            }
        });
        let mcp = driver
            .mcp
            .and_then(|m| serde_json::to_value(m).ok())
            .and_then(|m| m.as_str().map(str::to_owned));
        Some(crate::config::TuiAgent {
            command,
            profile,
            name: Some(egg.name.clone()),
            env: Default::default(),
            transcript,
            mcp,
            unrecognised_after_ms: None,
            size: std::env::var("PARROT_TUI_SIZE")
                .ok()
                .as_deref()
                .and_then(parse_size),
            no_guard: false,
        })
    }
}

/// `<rows>x<cols>`, such as `30x100`.
fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (rows, cols) = s.split_once('x')?;
    Some((rows.trim().parse().ok()?, cols.trim().parse().ok()?))
}

/// The TUI profile in an egg's `parrot` folder.
pub const PROFILE: &str = "profile.kdl";

fn agent_egg(egg: &LoadedEgg) -> AgentEgg {
    let (driver_name, kind, data, controls, cli_versions) = match &egg.driver {
        Ok(d) => {
            let (name, kind) = match &d.driver {
                DriverRun::Builtin { name } => (name.clone(), DriverKind::Builtin),
                DriverRun::Command { .. } => (egg.agent.clone(), DriverKind::Process),
            };
            let versions = d.cli.as_ref().and_then(|c| c.versions.clone());
            (name, kind, d.data_version.clone(), d.controls.clone(), versions)
        }
        Err(_) => (
            egg.agent.clone(),
            DriverKind::Builtin,
            String::new(),
            vec![Control::Native],
            None,
        ),
    };
    let driver_version = match kind {
        DriverKind::Builtin => env!("CARGO_PKG_VERSION").to_string(),
        DriverKind::Process => egg.version.clone(),
    };
    AgentEgg {
        id: egg.agent.clone(),
        name: egg.name.clone(),
        driver_name,
        driver_version,
        kind,
        data,
        controls,
        cli_versions,
        refused: egg.driver.as_ref().err().cloned(),
    }
}

fn parrot_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot find parrot: {e}"))
}
