//! Carrier eggs (spec 24.2): `carrier.kdl` in an egg's `parrot` folder names a program that
//! opens a link channel on its standard input and output.
//!
//! ```kdl
//! carrier "relay" title="Made-up relay" max-message=16384 containment="restricted"
//! run "bin/carrier"
//! target "{\"type\":\"object\"}"
//! ```
//!
//! | Node | Meaning |
//! | --- | --- |
//! | `carrier "kind"` | The carrier kind a `CarrierRef` names, with `title`, `max-message` (bytes, at least 4096), `containment` (`restricted`, `shell`) and `listens` |
//! | `run "path" args…` | The program, relative to the `parrot` folder, and its arguments |
//! | `target "json"` | The JSON Schema of the carrier's target |
//!
//! Containment left out is `unknown`, which clients treat as `shell` (spec 24.4).

use crate::kdlx;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The file in an egg's `parrot` folder that makes it a carrier egg.
pub const FILE: &str = "carrier.kdl";

/// A parsed `carrier.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct CarrierFile {
    pub kind: String,
    pub title: String,
    pub max_message: usize,
    /// `restricted` or `shell`; `None` when the egg doesn't say.
    pub containment: Option<String>,
    pub listens: bool,
    /// Relative to the `parrot` folder.
    pub run: String,
    pub args: Vec<String>,
    pub target: Value,
}

impl CarrierFile {
    pub fn parse(text: &str) -> Result<CarrierFile, Vec<String>> {
        let doc = kdlx::parse(text).map_err(|e| vec![e])?;
        let mut problems = Vec::new();
        let mut out = CarrierFile {
            kind: String::new(),
            title: String::new(),
            max_message: 0,
            containment: None,
            listens: false,
            run: String::new(),
            args: Vec::new(),
            target: serde_json::json!({ "type": "object" }),
        };
        for node in doc.nodes() {
            match node.name().value() {
                "carrier" => {
                    out.kind = kdlx::arg_str(node).unwrap_or_default().to_string();
                    out.title = kdlx::prop_str(node, "title").unwrap_or(&out.kind).to_string();
                    out.max_message = kdlx::prop_int(node, "max-message")
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(65_535);
                    out.containment = kdlx::prop_str(node, "containment").map(str::to_string);
                    out.listens = node
                        .entries()
                        .iter()
                        .find(|e| e.name().is_some_and(|n| n.value() == "listens"))
                        .and_then(|e| e.value().as_bool())
                        .unwrap_or(false);
                }
                "run" => {
                    let mut args = kdlx::args(node).filter_map(|v| v.as_string().map(str::to_string));
                    out.run = args.next().unwrap_or_default();
                    out.args = args.collect();
                }
                "target" => match kdlx::arg_str(node).map(serde_json::from_str::<Value>) {
                    Some(Ok(v)) if v.is_object() => out.target = v,
                    _ => problems.push("target needs a JSON Schema object as text".into()),
                },
                _ => {}
            }
        }
        let kind_ok = !out.kind.is_empty()
            && out
                .kind
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !kind_ok {
            problems.push("carrier needs a kind of lowercase letters, digits and -".into());
        }
        if out.max_message < 4096 {
            problems.push("max-message is at least 4096".into());
        }
        if let Some(c) = &out.containment
            && !["restricted", "shell"].contains(&c.as_str())
        {
            problems.push(format!("containment is restricted or shell, not {c}"));
        }
        let path = Path::new(&out.run);
        if out.run.is_empty() || path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
            problems.push("run needs a path inside the parrot folder".into());
        }
        if problems.is_empty() {
            Ok(out)
        } else {
            Err(problems)
        }
    }
}

/// A carrier egg found in an installed package.
#[derive(Debug, Clone)]
pub struct LoadedCarrier {
    pub package: String,
    pub data_dir: PathBuf,
    pub carrier: Result<CarrierFile, String>,
}

/// Whether a `parrot` folder holds a carrier egg rather than a driver.
pub fn is_carrier(data_dir: &Path) -> bool {
    data_dir.join(FILE).is_file()
}

/// Reads `carrier.kdl` in a `parrot` folder.
pub fn read(data_dir: &Path) -> Result<CarrierFile, String> {
    let text =
        std::fs::read_to_string(data_dir.join(FILE)).map_err(|e| format!("cannot read {FILE}: {e}"))?;
    CarrierFile::parse(&text).map_err(|p| p.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_carrier_file() {
        let c = CarrierFile::parse(
            "carrier \"relay\" title=\"Made-up relay\" max-message=16384 containment=\"restricted\"\nrun \"bin/carrier\" \"--quiet\"\ntarget \"{\\\"type\\\":\\\"object\\\"}\"\n",
        )
        .unwrap();
        assert_eq!(c.kind, "relay");
        assert_eq!(c.max_message, 16384);
        assert_eq!(c.containment.as_deref(), Some("restricted"));
        assert_eq!(c.run, "bin/carrier");
        assert_eq!(c.args, ["--quiet"]);
    }

    #[test]
    fn refuses_escapes_and_small_messages() {
        let e = CarrierFile::parse("carrier \"Bad Kind\" max-message=10\nrun \"../x\"\n").unwrap_err();
        assert_eq!(e.len(), 3, "{e:?}");
    }
}
