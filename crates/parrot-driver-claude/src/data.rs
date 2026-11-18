//! The Claude egg's data files (spec 31), read from `$PARROT_DRIVER_DATA` with the copies in
//! `eggs/claude/parrot` compiled in as the fallback.

use std::sync::OnceLock;

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

static DATA: OnceLock<(DriverData, Option<String>)> = OnceLock::new();

/// The version `claude --version` reports, which picks the egg's version shim (spec 31).
fn cli_version() -> Option<String> {
    let bin = std::env::var_os(crate::driver::CLAUDE_BIN_ENV).unwrap_or_else(|| "claude".into());
    let out = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_owned)
}

fn loaded() -> &'static (DriverData, Option<String>) {
    DATA.get_or_init(|| match DriverData::load_for(BUILTIN, cli_version().as_deref()) {
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
