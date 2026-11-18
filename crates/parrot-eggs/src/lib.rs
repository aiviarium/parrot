//! Driver eggs: the `parrot` Hatchery target, installed drivers, their data files and how each
//! runs (a built-in core or a command).
//!
//! | Module | Holds |
//! | --- | --- |
//! | [`target`] | The `parrot` target: the fact `parrot=<version>` and the check of a target's files |
//! | [`driver`] | `driver.kdl` |
//! | [`build`] | Building a command egg's driver and putting it in its variant folder |
//! | [`carrier`] | `carrier.kdl`, for carrier eggs |
//! | [`probe`] | Probe scenarios, `probes/*.kdl` |
//! | [`store`] | Installing into `<state_dir>/eggs`, packing, and loading |
//! | [`signing`] | Signing a static registry |
//! | [`schema`] | The JSON Schemas of the data files |

pub mod build;
pub mod carrier;
pub mod driver;
mod kdlx;
pub mod probe;
pub mod schema;
pub mod signing;
pub mod store;
pub mod target;

pub use driver::{CliSpec, CommandVariant, DriverFile, DriverRun, ShimSpec};
pub use probe::{Probe, ProbeStep};
pub use store::{Eggs, LoadedEgg};
pub use target::{ParrotTarget, TARGET};

/// The runtime version this crate answers for the `parrot` fact.
pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The variable that names an egg's `parrot` folder for its driver.
pub const DATA_ENV: &str = parrot_proto::driver_data::DRIVER_DATA_ENV;

/// The variables that name the agent a driver serves: its id and display name.
pub use parrot_proto::driver_data::{AGENT_ENV, AGENT_NAME_ENV};

/// The registry `parrot eggs install` uses when no `--registry` is given: the static registry
/// published with each Parrot release. `PARROT_EGGS_REGISTRY` overrides it.
pub const DEFAULT_REGISTRY: &str = "https://aiviarium.github.io/parrot/eggs";

/// The variable that replaces [`DEFAULT_REGISTRY`].
pub const REGISTRY_ENV: &str = "PARROT_EGGS_REGISTRY";
