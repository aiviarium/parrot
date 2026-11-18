//! The in-repo eggs, compiled in: the fallback when nothing is installed, and the data a
//! built-in driver reads when it runs without `PARROT_DRIVER_DATA`.

use std::collections::BTreeMap;

include!(concat!(env!("OUT_DIR"), "/builtin.rs"));

/// Each compiled-in package by id, with its files by path relative to the package folder.
pub fn packages() -> BTreeMap<&'static str, BTreeMap<String, &'static [u8]>> {
    let mut out: BTreeMap<&str, BTreeMap<String, &[u8]>> = BTreeMap::new();
    for (path, bytes) in FILES {
        if let Some((id, rest)) = path.split_once('/') {
            out.entry(id).or_default().insert(rest.to_string(), bytes);
        }
    }
    out
}

/// One file of a compiled-in egg's `parrot` folder, such as `("claude", "options.kdl")`.
pub fn data_file(agent: &str, file: &str) -> Option<&'static [u8]> {
    let path = format!("{agent}/parrot/{file}");
    FILES.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}
