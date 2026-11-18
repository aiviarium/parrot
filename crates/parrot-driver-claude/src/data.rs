//! The Claude egg's data files (spec 31), read from `$PARROT_DRIVER_DATA` with the copies in
//! `eggs/claude/parrot` compiled in as the fallback. The version Claude Code reports picks the
//! version shim.

use parrot_driver::data::DataCell;
use parrot_proto::driver_data::{Builtin, DriverData};

pub const BUILTIN: Builtin = Builtin {
    driver: include_str!("../../../eggs/claude/parrot/driver.kdl"),
    options: include_str!("../../../eggs/claude/parrot/options.kdl"),
    levels: include_str!("../../../eggs/claude/parrot/levels.kdl"),
    correlate: include_str!("../../../eggs/claude/parrot/correlate.kdl"),
    shims: &[(
        "shims/2.1.325",
        "options.kdl",
        include_str!("../../../eggs/claude/parrot/shims/2.1.325/options.kdl"),
    )],
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

/// Takes the version Claude Code reported, from `--version` or its `initialize` answer, and
/// returns whether it picked another version shim.
pub fn observe_version(version: &str) -> bool {
    DATA.observe(version)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_data_parses() {
        let d = parrot_proto::driver_data::DriverData::builtin(super::BUILTIN).unwrap();
        assert!(d.options.get("permissionMode").is_some());
        assert!(!d.levels.map.is_empty());
        assert_eq!(d.correlate.winner("tool", true), "native");
    }
}
