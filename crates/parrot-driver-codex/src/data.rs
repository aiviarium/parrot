//! The Codex egg's data files (spec 31), read from `$PARROT_DRIVER_DATA` with the copies in
//! `eggs/codex/parrot` compiled in as the fallback, for the Codex version the driver has seen.

use parrot_driver::data::DataCell;
use parrot_proto::driver_data::{Builtin, DriverData};

pub const BUILTIN: Builtin = Builtin {
    driver: include_str!("../../../eggs/codex/parrot/driver.kdl"),
    options: include_str!("../../../eggs/codex/parrot/options.kdl"),
    levels: include_str!("../../../eggs/codex/parrot/levels.kdl"),
    correlate: include_str!("../../../eggs/codex/parrot/correlate.kdl"),
    shims: &[],
};

static DATA: DataCell = DataCell::new(BUILTIN);

/// The driver data in use.
pub fn data() -> &'static DriverData {
    DATA.get()
}

/// Why the egg's data was not used, if it wasn't.
pub fn load_error() -> Option<String> {
    DATA.load_error()
}

/// Takes the Codex version from `--version` or the `initialize` answer, which picks the egg's
/// version shim (spec 31). Returns whether the data changed.
pub fn observe_version(version: &str) -> bool {
    DATA.observe(version)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_data_parses() {
        let d = parrot_proto::driver_data::DriverData::builtin(super::BUILTIN).unwrap();
        assert!(d.options.get("sandbox").is_some());
        assert!(!d.levels.map.is_empty());
        assert_eq!(d.correlate.winner("tool", true), "native");
    }
}
