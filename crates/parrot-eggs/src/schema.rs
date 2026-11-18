//! The JSON Schemas of the driver data files this crate reads. Each describes the file as it
//! reads into JSON; they are committed under `spec/schema/driver-data/`.

use serde_json::Value;

/// `(file name, schema)` for each data file: `driver.schema.json` and `probe.schema.json`.
pub fn all() -> Vec<(&'static str, Value)> {
    vec![
        ("driver.schema.json", one::<crate::DriverFile>("driver.kdl")),
        ("probe.schema.json", one::<crate::Probe>("probes/*.kdl")),
    ]
}

fn one<T: schemars::JsonSchema>(file: &str) -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).expect("schemas serialize");
    v["description"] = Value::String(format!(
        "The driver data file {file}, read from KDL into this shape."
    ));
    v
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    fn spec() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec")
    }

    /// Fails when a committed schema is stale; `PARROT_BLESS=1` rewrites them.
    #[test]
    fn committed_schemas_are_current() {
        let dir = spec().join("schema/driver-data");
        for (name, schema) in super::all() {
            let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
            let path = dir.join(name);
            if std::env::var_os("PARROT_BLESS").is_some() {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&path, &text).unwrap();
                continue;
            }
            let have = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                have == text,
                "{} is stale; run with PARROT_BLESS=1",
                path.display()
            );
        }
    }

    #[test]
    fn the_examples_are_valid() {
        let dir = spec().join("examples/driver-data");
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        crate::DriverFile::parse(&read("driver.kdl")).unwrap();
        crate::Probe::parse(&read("probe.kdl")).unwrap();
    }

    #[test]
    fn the_in_repo_eggs_are_valid() {
        let eggs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eggs");
        let target = crate::ParrotTarget::default();
        for id in ["claude", "codex", "acp", "pi", "opencode", "t3"] {
            let dir = eggs.join(id);
            let problems = crate::store::check_package(&dir, &target);
            assert!(problems.is_empty(), "{id}: {problems:?}");
            let manifest = hatchery::Manifest::load(&dir).unwrap();
            assert_eq!(manifest.name().id, id);
        }
    }
}
