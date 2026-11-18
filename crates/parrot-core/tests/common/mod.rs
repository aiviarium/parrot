//! What the core tests share: a core with the agents a test runtime gets from its eggs.

use parrot_core::Core;
use parrot_core::agents::AgentEgg;
use parrot_core::clock::{Clock, Ids};
use parrot_proto::agent::{Control, DriverKind};

/// The agents the tests use, as installed eggs would describe them.
pub const AGENTS: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("pi", "Pi"),
    ("opencode", "opencode"),
];

/// An egg for `id`, driven natively.
pub fn egg(id: &str, name: &str) -> AgentEgg {
    AgentEgg {
        id: id.into(),
        name: name.into(),
        driver_name: id.into(),
        driver_version: "1".into(),
        kind: DriverKind::Process,
        data: "1".into(),
        controls: vec![Control::Native],
        cli_versions: None,
        refused: None,
        tool_timeout: None,
    }
}

/// A core with [`AGENTS`] loaded.
pub fn core(clock: Box<dyn Clock>, ids: Box<dyn Ids>) -> Core {
    let mut core = Core::new(clock, ids);
    parrot_core::agents::load(
        &mut core.state,
        AGENTS.iter().map(|(id, name)| egg(id, name)).collect(),
    );
    core
}
