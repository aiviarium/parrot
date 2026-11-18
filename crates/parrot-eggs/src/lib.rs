//! Driver eggs: the `parrot` Hatchery target, the in-repo registry in `eggs/`, installed drivers,
//! their data files and how each runs (built in or as a command).
//!
//! | Module | Holds |
//! | --- | --- |
//! | [`target`] | The `parrot` target: the fact `parrot=<version>` and the check of a target's files |
//! | [`driver`] | `driver.kdl` |
//! | [`carrier`] | `carrier.kdl`, for carrier eggs |
//! | [`probe`] | Probe scenarios, `probes/*.kdl` |
//! | [`store`] | Installing into `<state_dir>/eggs`, the compiled-in fallback, and loading |
//! | [`builtin`] | The in-repo eggs, compiled in |
//! | [`schema`] | The JSON Schemas of the data files |

pub mod builtin;
pub mod carrier;
pub mod driver;
mod kdlx;
pub mod probe;
pub mod schema;
pub mod store;
pub mod target;

pub use driver::{CliSpec, CommandVariant, DriverFile, DriverRun};
pub use probe::{Probe, ProbeStep};
pub use store::{EggSource, Eggs, LoadedEgg};
pub use target::{ParrotTarget, TARGET};

/// The runtime version this crate answers for the `parrot` fact.
pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The variable that names an egg's `parrot` folder for its driver.
pub const DATA_ENV: &str = "PARROT_DRIVER_DATA";

/// A file of this driver's data: from `PARROT_DRIVER_DATA` when it is set, otherwise from the
/// copy of `agent`'s egg compiled into the runtime.
pub fn data_file(agent: &str, file: &str) -> Option<Vec<u8>> {
    match std::env::var_os(DATA_ENV).filter(|v| !v.is_empty()) {
        Some(dir) => std::fs::read(std::path::Path::new(&dir).join(file)).ok(),
        None => builtin::data_file(agent, file).map(<[u8]>::to_vec),
    }
}
