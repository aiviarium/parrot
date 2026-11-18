//! `driver.kdl`: how a driver runs, which CLI versions it covers, and its data version.
//!
//! ```kdl
//! driver "builtin" name="claude"
//! cli "claude" versions=">=2.0"
//! data-version "1"
//! controls "native" "tui"
//! ```
//!
//! ```kdl
//! driver "command" {
//!   run "variants/{os}-{arch}/driver{exe}"
//! }
//! cli "example" versions=">=1.0"
//! data-version "1"
//! ```
//!
//! Nodes this version doesn't know are ignored, so a newer egg still loads.

use crate::kdlx;
use parrot_proto::agent::Control;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The contents of `driver.kdl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriverFile {
    /// How the driver runs.
    pub driver: DriverRun,
    /// The harness CLI the driver talks to, and the versions this data covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliSpec>,
    /// The driver data version, reported as `driver.data` on the agent (spec 6).
    pub data_version: String,
    /// How the agent can be driven; the first is the default. `native` when left out.
    #[serde(default = "native_only")]
    pub controls: Vec<Control>,
    /// How many bindings one driver process serves; any number when left out (spec 30.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<u32>,
    /// Version shims: folders whose files replace the egg's own for a range of harness
    /// versions. The last listed range that holds the detected version wins (spec 31).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shims: Vec<ShimSpec>,
    /// Where the TUI core reads the harness's own transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptSpec>,
    /// How the TUI core hands the harness the `parrot` MCP server (spec 26).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpStyle>,
}

/// `transcript "claude"`, `transcript "codex"` or `transcript "jsonl" dir="<folder>"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSpec {
    pub format: TranscriptFormat,
    /// The folder to watch, in place of the format's own. `~` is the home folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// The transcript formats the TUI core reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptFormat {
    /// Claude Code's project transcripts, in the folder for the working directory.
    Claude,
    /// Codex rollout files under `sessions/`.
    Codex,
    /// Plain JSONL messages in `dir`.
    Jsonl,
}

/// How the TUI core passes the `parrot` MCP server to the harness. The token is only ever in
/// the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum McpStyle {
    /// `--mcp-config <json>`.
    McpConfig,
    /// `-c mcp_servers.parrot.*` config overrides.
    ConfigOverride,
}

/// One `shim ">=2.2" dir="shims/2.2"` line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShimSpec {
    /// A semver range of harness versions.
    pub versions: String,
    /// The folder, relative to the egg's `parrot` folder.
    pub dir: String,
}

fn native_only() -> Vec<Control> {
    vec![Control::Native]
}

/// How a driver runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DriverRun {
    /// A driver compiled into the runtime, served by `parrot driver <name>`.
    Builtin { name: String },
    /// A program in the egg's `parrot` folder, with a variant per platform when it is a binary.
    Command {
        /// The program, relative to the egg's `parrot` folder, when no variant matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        /// Arguments passed to the program.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Per-platform programs; the first that matches wins.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        variants: Vec<CommandVariant>,
    },
}

/// One platform's program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandVariant {
    /// The OS as Rust names it (`linux`, `macos`, `windows`); any when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// The architecture as Rust names it (`x86_64`, `aarch64`); any when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// The program, relative to the egg's `parrot` folder.
    pub run: String,
}

/// The harness CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CliSpec {
    /// The command name, such as `claude`.
    pub name: String,
    /// A semver range of the CLI versions this data covers, such as `>=2.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versions: Option<String>,
}

impl DriverFile {
    /// Reads `driver.kdl`; the error lists every problem found.
    pub fn parse(text: &str) -> Result<DriverFile, Vec<String>> {
        let doc = kdlx::parse(text).map_err(|e| vec![e])?;
        let mut problems = Vec::new();
        let mut driver = None;
        let mut cli = None;
        let mut data_version = None;
        let mut controls = None;
        let mut bindings = None;
        let mut shims = Vec::new();
        let mut transcript = None;
        let mut mcp = None;
        for node in doc.nodes() {
            match node.name().value() {
                "driver" => driver = parse_run(node, &mut problems),
                "cli" => match kdlx::arg_str(node) {
                    Some(name) => {
                        let versions = kdlx::prop_str(node, "versions").map(str::to_string);
                        if let Some(v) = &versions
                            && hatchery::semver::VersionReq::parse(v).is_err()
                        {
                            problems.push(format!("cli versions {v:?} is not a semver range"));
                        }
                        cli = Some(CliSpec {
                            name: name.to_string(),
                            versions,
                        });
                    }
                    None => problems.push("cli needs the command name".into()),
                },
                "data-version" => match kdlx::args(node).next() {
                    Some(v) => data_version = Some(kdlx::text(v)),
                    None => problems.push("data-version needs a value".into()),
                },
                "controls" => {
                    let mut list = Vec::new();
                    for v in kdlx::args(node) {
                        match v.as_string() {
                            Some("native") => list.push(Control::Native),
                            Some("tui") => list.push(Control::Tui),
                            _ => {
                                problems.push(format!("controls takes native and tui, not {}", kdlx::text(v)))
                            }
                        }
                    }
                    if list.is_empty() {
                        problems.push("controls needs at least one control".into());
                    }
                    controls = Some(list);
                }
                "shim" => match (kdlx::arg_str(node), kdlx::prop_str(node, "dir")) {
                    (Some(v), Some(dir)) => {
                        if hatchery::semver::VersionReq::parse(v).is_err() {
                            problems.push(format!("shim versions {v:?} is not a semver range"));
                        }
                        shims.push(ShimSpec {
                            versions: v.to_string(),
                            dir: dir.to_string(),
                        });
                    }
                    _ => problems.push("shim needs a version range and dir=".into()),
                },
                "transcript" => {
                    let dir = kdlx::prop_str(node, "dir").map(str::to_string);
                    let format = match kdlx::arg_str(node) {
                        Some("claude") => Some(TranscriptFormat::Claude),
                        Some("codex") => Some(TranscriptFormat::Codex),
                        Some("jsonl") if dir.is_some() => Some(TranscriptFormat::Jsonl),
                        Some("jsonl") => {
                            problems.push("transcript \"jsonl\" needs dir=".into());
                            None
                        }
                        other => {
                            problems.push(format!("transcript takes claude, codex or jsonl, not {other:?}"));
                            None
                        }
                    };
                    transcript = format.map(|format| TranscriptSpec { format, dir });
                }
                "mcp" => match kdlx::arg_str(node) {
                    Some("mcp-config") => mcp = Some(McpStyle::McpConfig),
                    Some("config-override") => mcp = Some(McpStyle::ConfigOverride),
                    other => problems.push(format!("mcp takes mcp-config or config-override, not {other:?}")),
                },
                "bindings" => match kdlx::args(node).next().and_then(|v| v.as_integer()) {
                    Some(n) if n >= 1 && n <= i128::from(u32::MAX) => bindings = Some(n as u32),
                    _ => problems.push("bindings needs a positive number".into()),
                },
                _ => {}
            }
        }
        if driver.is_none() && !problems.iter().any(|p| p.starts_with("driver")) {
            problems.push("driver.kdl has no driver node".into());
        }
        if data_version.is_none() {
            problems.push("driver.kdl has no data-version".into());
        }
        match (driver, data_version) {
            (Some(driver), Some(data_version)) if problems.is_empty() => Ok(DriverFile {
                driver,
                cli,
                data_version,
                controls: controls.unwrap_or_else(native_only),
                bindings,
                shims,
                transcript,
                mcp,
            }),
            _ => Err(problems),
        }
    }

    /// The command to run on `os`/`arch`, relative to the egg's `parrot` folder, with its
    /// arguments. `None` for a built-in driver. In the path, `{os}` and `{arch}` become the
    /// platform's names as Rust gives them, and `{exe}` becomes `.exe` on Windows.
    pub fn command_for(&self, os: &str, arch: &str) -> Option<Result<(String, Vec<String>), String>> {
        let DriverRun::Command { run, args, variants } = &self.driver else {
            return None;
        };
        let matches = |v: &CommandVariant| {
            v.os.as_deref().is_none_or(|o| o == os) && v.arch.as_deref().is_none_or(|a| a == arch)
        };
        let program = variants
            .iter()
            .find(|v| matches(v))
            .map(|v| v.run.clone())
            .or_else(|| run.clone());
        let exe = if os == "windows" { ".exe" } else { "" };
        let fill = |p: &str| {
            p.replace("{os}", os)
                .replace("{arch}", arch)
                .replace("{exe}", exe)
        };
        Some(match program {
            Some(p) => Ok((fill(&p), args.clone())),
            None => Err(format!("the driver has no program for {os}-{arch}")),
        })
    }
}

fn parse_run(node: &kdl::KdlNode, problems: &mut Vec<String>) -> Option<DriverRun> {
    match kdlx::arg_str(node) {
        Some("builtin") => match kdlx::prop_str(node, "name") {
            Some(name) => Some(DriverRun::Builtin { name: name.into() }),
            None => {
                problems.push("driver \"builtin\" needs name=".into());
                None
            }
        },
        Some("command") => {
            let mut run = kdlx::prop_str(node, "run").map(str::to_string);
            let mut args = Vec::new();
            let mut variants = Vec::new();
            for child in kdlx::children(node) {
                match child.name().value() {
                    "run" => match kdlx::arg_str(child) {
                        Some(p) => {
                            check_path(p, problems);
                            run = Some(p.into());
                            args = kdlx::args(child).skip(1).map(kdlx::text).collect();
                        }
                        None => problems.push("driver run needs a path".into()),
                    },
                    "variant" => match kdlx::prop_str(child, "run") {
                        Some(p) => {
                            check_path(p, problems);
                            variants.push(CommandVariant {
                                os: kdlx::prop_str(child, "os").map(str::to_string),
                                arch: kdlx::prop_str(child, "arch").map(str::to_string),
                                run: p.into(),
                            });
                        }
                        None => problems.push("driver variant needs run=".into()),
                    },
                    other => problems.push(format!("driver \"command\" has an unknown node {other}")),
                }
            }
            if run.is_none() && variants.is_empty() {
                problems.push("driver \"command\" needs run or a variant".into());
            }
            Some(DriverRun::Command { run, args, variants })
        }
        Some(other) => {
            problems.push(format!("driver kind {other:?} is neither builtin nor command"));
            None
        }
        None => {
            problems.push("driver needs a kind: builtin or command".into());
            None
        }
    }
}

/// Programs stay inside the egg.
fn check_path(p: &str, problems: &mut Vec<String>) {
    let path = std::path::Path::new(p);
    let escapes = path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)));
    if escapes || p.contains('\\') {
        problems.push(format!(
            "driver program {p:?} must be a relative path inside the egg"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_builtin_driver() {
        let d = DriverFile::parse(
            "driver \"builtin\" name=\"claude\"\ncli \"claude\" versions=\">=2.0\"\ndata-version \"1\"\ncontrols \"native\" \"tui\"\n",
        )
        .unwrap();
        assert_eq!(
            d.driver,
            DriverRun::Builtin {
                name: "claude".into()
            }
        );
        assert_eq!(d.cli.unwrap().versions.as_deref(), Some(">=2.0"));
        assert_eq!(d.controls, vec![Control::Native, Control::Tui]);
    }

    #[test]
    fn picks_the_variant_for_the_platform() {
        let d = DriverFile::parse(
            r#"driver "command" {
  run "bin/driver" "--stdio"
  variant os="linux" arch="x86_64" run="bin/linux-x86_64/driver"
  variant os="windows" run="bin/driver.exe"
}
data-version "1"
"#,
        )
        .unwrap();
        let pick = |os, arch| d.command_for(os, arch).unwrap().unwrap().0;
        assert_eq!(pick("linux", "x86_64"), "bin/linux-x86_64/driver");
        assert_eq!(pick("windows", "aarch64"), "bin/driver.exe");
        assert_eq!(pick("macos", "aarch64"), "bin/driver");
        assert_eq!(
            d.command_for("macos", "aarch64").unwrap().unwrap().1,
            vec!["--stdio"]
        );
    }

    #[test]
    fn reads_the_tui_core_nodes() {
        let d = DriverFile::parse(
            "driver \"builtin\" name=\"tui\"\ndata-version \"1\"\ntranscript \"codex\"\nmcp \"config-override\"\n",
        )
        .unwrap();
        assert_eq!(d.transcript.as_ref().unwrap().format, TranscriptFormat::Codex);
        assert_eq!(d.mcp, Some(McpStyle::ConfigOverride));
        let d = DriverFile::parse(
            "driver \"builtin\" name=\"tui\"\ndata-version \"1\"\ntranscript \"jsonl\" dir=\"~/.fake/log\"\n",
        )
        .unwrap();
        assert_eq!(d.transcript.unwrap().dir.as_deref(), Some("~/.fake/log"));
        let e = DriverFile::parse(
            "driver \"builtin\" name=\"tui\"\ndata-version \"1\"\ntranscript \"jsonl\"\nmcp \"telepathy\"\n",
        )
        .unwrap_err();
        assert_eq!(e.len(), 2, "{e:?}");
    }

    #[test]
    fn lists_every_problem() {
        let e = DriverFile::parse(
            "driver \"command\" { run \"../escape\"; }\ncli \"x\" versions=\"not a range\"\n",
        )
        .unwrap_err();
        assert_eq!(e.len(), 3, "{e:?}");
        assert!(DriverFile::parse("driver {").unwrap_err()[0].contains("not valid KDL"));
    }

    #[test]
    fn fills_the_platform_into_a_command() {
        let d = DriverFile::parse(
            "driver \"command\" {\n  run \"variants/{os}-{arch}/d{exe}\"\n}\ndata-version \"1\"\n",
        )
        .unwrap();
        let (run, _) = d.command_for("windows", "x86_64").unwrap().unwrap();
        assert_eq!(run, "variants/windows-x86_64/d.exe");
        let (run, _) = d.command_for("linux", "aarch64").unwrap().unwrap();
        assert_eq!(run, "variants/linux-aarch64/d");
    }
}
