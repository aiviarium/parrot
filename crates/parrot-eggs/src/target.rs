//! The `parrot` Hatchery target.

use crate::driver::{DriverFile, DriverRun};
use crate::probe::Probe;
use hatchery::{Problem, Target, TargetFiles};

/// The target name under `targets` in `package.kdl`.
pub const TARGET: &str = "parrot";

/// The runtime as a Hatchery target: it answers the fact `parrot=<version>` and checks the
/// driver data of each egg it installs or packs.
#[derive(Debug, Clone)]
pub struct ParrotTarget {
    version: String,
}

impl ParrotTarget {
    /// The target for a runtime of `version`.
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
        }
    }

    /// The version it answers for the `parrot` fact.
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl Default for ParrotTarget {
    fn default() -> Self {
        Self::new(crate::RUNTIME_VERSION)
    }
}

impl Target for ParrotTarget {
    fn name(&self) -> &str {
        TARGET
    }

    fn fact(&self, name: &str) -> Option<String> {
        (name == TARGET).then(|| self.version.clone())
    }

    fn check(&self, files: &TargetFiles) -> Result<(), Vec<Problem>> {
        let problems = check_files(files.paths().map(|p| (p, files.get(p).unwrap_or_default())));
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

/// Checks the files of a `parrot` folder, by path relative to it: `driver.kdl` must be valid,
/// probes must be valid scenarios, and every other `.kdl` file must parse as KDL.
pub fn check_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Vec<Problem> {
    let files: Vec<(&str, &[u8])> = files.into_iter().collect();
    let mut problems = Vec::new();
    let text_of = |path: &str, bytes: &[u8], problems: &mut Vec<Problem>| match std::str::from_utf8(bytes) {
        Ok(t) => Some(t.to_string()),
        Err(_) => {
            problems.push(Problem::in_file(path, "is not UTF-8"));
            None
        }
    };
    let mut driver = None;
    for (path, bytes) in &files {
        if !path.ends_with(".kdl") || path.starts_with("variants/") {
            continue;
        }
        let Some(text) = text_of(path, bytes, &mut problems) else {
            continue;
        };
        let result = if *path == "driver.kdl" {
            DriverFile::parse(&text).map(|d| driver = Some(d))
        } else if path.starts_with("probes/") {
            Probe::parse(&text).map(drop)
        } else {
            crate::kdlx::parse(&text).map(drop).map_err(|e| vec![e])
        };
        if let Err(errs) = result {
            problems.extend(errs.into_iter().map(|e| Problem::in_file(*path, e)));
        }
    }
    match driver {
        None if !problems.iter().any(|p| p.path.as_deref() == Some("driver.kdl")) => {
            problems.push(Problem::new("the parrot folder has no driver.kdl"));
        }
        Some(DriverFile {
            driver: DriverRun::Command { run, variants, .. },
            ..
        }) => {
            // Programs in variant folders arrive with the variant egg, so only shared ones are checked.
            let programs = run.iter().chain(variants.iter().map(|v| &v.run));
            for p in programs {
                if !p.starts_with("variants/") && !files.iter().any(|(f, _)| f == p) {
                    problems.push(Problem::in_file(
                        "driver.kdl",
                        format!("the program {p} is not in the egg"),
                    ));
                }
            }
        }
        _ => {}
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_its_own_fact() {
        let t = ParrotTarget::new("0.1.0");
        assert_eq!(t.fact("parrot").as_deref(), Some("0.1.0"));
        assert_eq!(t.fact("crow"), None);
    }

    #[test]
    fn checks_the_data_files() {
        let ok = check_files([
            (
                "driver.kdl",
                b"driver \"builtin\" name=\"x\"\ndata-version \"1\"\n".as_slice(),
            ),
            ("options.kdl", b"option \"model\" role=\"model\"".as_slice()),
            ("probes/hello.kdl", b"prompt \"hi\"".as_slice()),
        ]);
        assert!(ok.is_empty(), "{ok:?}");
        let bad = check_files([
            (
                "driver.kdl",
                b"driver \"command\" { run \"bin/driver\"; }\ndata-version \"1\"\n".as_slice(),
            ),
            ("levels.kdl", b"level {".as_slice()),
            ("probes/x.kdl", b"jump".as_slice()),
        ]);
        let paths: Vec<_> = bad.iter().map(|p| p.path.clone().unwrap_or_default()).collect();
        assert_eq!(paths, ["levels.kdl", "probes/x.kdl", "driver.kdl"], "{bad:?}");
        assert!(!check_files([("options.kdl", b"".as_slice())]).is_empty());
    }
}
