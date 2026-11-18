//! The Claude egg's data files (spec 31), read from `$PARROT_DRIVER_DATA` with the copies in
//! `eggs/claude/parrot` compiled in as the fallback.

use std::sync::OnceLock;

use parrot_proto::driver_data::{Builtin, DriverData};

pub const BUILTIN: Builtin = Builtin {
    driver: "",
    options: include_str!("../../../eggs/claude/parrot/options.kdl"),
    levels: include_str!("../../../eggs/claude/parrot/levels.kdl"),
    correlate: include_str!("../../../eggs/claude/parrot/correlate.kdl"),
};

static DATA: OnceLock<(DriverData, Option<String>)> = OnceLock::new();

fn loaded() -> &'static (DriverData, Option<String>) {
    DATA.get_or_init(|| match DriverData::load(BUILTIN) {
        Ok(d) => (d, None),
        Err(e) => (
            DriverData::builtin(BUILTIN).expect("the compiled-in data parses"),
            Some(format!(
                "the egg's driver data is invalid, using the built-in copy: {e}"
            )),
        ),
    })
}

/// The driver data in use.
pub fn data() -> &'static DriverData {
    &loaded().0
}

/// Why the egg's data was not used, if it wasn't.
pub fn load_error() -> Option<&'static str> {
    loaded().1.as_deref()
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
