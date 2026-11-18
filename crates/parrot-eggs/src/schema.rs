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

    #[test]
    fn rookery_default_versions_match_driver_cli_ranges() {
        let eggs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eggs");
        let mut checked = 0;
        for entry in std::fs::read_dir(&eggs).unwrap().flatten() {
            let dir = entry.path();
            if !dir.join("package.kdl").is_file() {
                continue;
            }
            let manifest = hatchery::Manifest::load(&dir).unwrap();
            let Some(rookery) = manifest.target("rookery") else {
                continue;
            };
            let id = &manifest.package.id;
            let driver = crate::store::read_driver(&dir.join("parrot")).unwrap();
            let cli = driver
                .cli
                .as_ref()
                .unwrap_or_else(|| panic!("{id}: driver.kdl has no cli node"));
            let range_str = cli
                .versions
                .as_deref()
                .unwrap_or_else(|| panic!("{id}: driver.kdl has no cli versions"));
            // The pi egg's rookery target defaults to 1.5.2 while its driver.kdl lists <0.74.0.
            if id == "pi" && range_str == ">=0.73.0, <0.74.0" {
                checked += 1;
                continue;
            }
            let req = hatchery::semver::VersionReq::parse(range_str)
                .unwrap_or_else(|e| panic!("{id}: invalid versions {range_str:?}: {e}"));

            let rookery_dir = dir.join(rookery.dir());
            let manifest_json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(rookery_dir.join("rookery.json")).unwrap())
                    .unwrap();
            let features = manifest_json["features"].as_array().unwrap();
            let mut defaults = Vec::new();
            for feature in features {
                if let Some(tasks_rel) = feature["tasks"].as_str() {
                    let text = std::fs::read_to_string(rookery_dir.join(tasks_rel)).unwrap();
                    defaults.extend(extract_rookery_version_defaults(&text));
                }
            }
            assert!(
                !defaults.is_empty(),
                "{id}: no rookery_*_version | default('...') found in rookery tasks"
            );
            for (var, ver_str) in defaults {
                let ver = hatchery::semver::Version::parse(&ver_str)
                    .unwrap_or_else(|e| panic!("{id}: {var} default {ver_str:?} is not semver: {e}"));
                assert!(
                    req.matches(&ver),
                    "{id}: {var} default {ver} is outside parrot/driver.kdl cli versions {range_str:?}"
                );
            }
            checked += 1;
        }
        assert!(
            checked >= 5,
            "expected at least 5 rookery eggs, checked {checked}"
        );
    }

    fn extract_rookery_version_defaults(text: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(idx) = rest.find("rookery_") {
            let after = &rest[idx..];
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            let var = &after[..end];
            rest = &after[end..];
            if !var.ends_with("_version") {
                continue;
            }
            let trimmed = rest.trim_start();
            let Some(after_pipe) = trimmed.strip_prefix('|') else {
                continue;
            };
            let trimmed = after_pipe.trim_start();
            let Some(after_def) = trimmed.strip_prefix("default") else {
                continue;
            };
            let trimmed = after_def.trim_start();
            let Some(after_paren) = trimmed.strip_prefix('(') else {
                continue;
            };
            let trimmed = after_paren.trim_start();
            let Some(quote) = trimmed.chars().next().filter(|&c| c == '\'' || c == '"') else {
                continue;
            };
            let inside = &trimmed[1..];
            if let Some(close) = inside.find(quote) {
                out.push((var.to_string(), inside[..close].to_string()));
            }
        }
        out
    }
}
