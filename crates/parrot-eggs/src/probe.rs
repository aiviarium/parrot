//! Probe scenarios, `probes/<name>.kdl`: steps run against a real CLI through its driver to
//! record frames and transcripts for new fixtures. They never run in CI.
//!
//! ```kdl
//! description "Ask for one word and wait for the turn to end."
//! control "native"
//! timeout-secs 120
//! option "model" "default"
//! prompt "Reply with the single word: hello."
//! wait "idle" timeout-secs=60
//! ```

use crate::kdlx;
use parrot_proto::agent::Control;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One probe scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// How the binding is driven; `native` when left out.
    #[serde(default = "native")]
    pub control: Control,
    /// The longest the whole scenario may take, in seconds. 300 when left out.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Option values for `binding/start`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
    /// The steps, in order.
    pub steps: Vec<ProbeStep>,
}

fn native() -> Control {
    Control::Native
}

fn default_timeout() -> u64 {
    300
}

/// One step of a probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum ProbeStep {
    /// Send a prompt.
    Prompt { text: String },
    /// Wait until the binding reaches a state, such as `idle`, or raises a request (`request`).
    Wait {
        until: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    /// Answer the newest open request with a choice id.
    Answer { choice: String },
    /// Cancel the running turn.
    Cancel,
    /// Type text and press keys (TUI control).
    Keys {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        press: Vec<String>,
    },
    /// Do nothing for a while.
    Sleep { ms: u64 },
}

impl Probe {
    /// Reads a probe scenario; the error lists every problem found.
    pub fn parse(text: &str) -> Result<Probe, Vec<String>> {
        let doc = kdlx::parse(text).map_err(|e| vec![e])?;
        let mut problems = Vec::new();
        let mut probe = Probe {
            description: None,
            control: Control::Native,
            timeout_secs: default_timeout(),
            options: BTreeMap::new(),
            steps: Vec::new(),
        };
        let secs = |v: Option<i128>| v.and_then(|n| u64::try_from(n).ok());
        for node in doc.nodes() {
            let name = node.name().value();
            let arg = kdlx::arg_str(node).map(str::to_string);
            let mut need = |what: &str| {
                problems.push(format!("{name} needs {what}"));
            };
            match name {
                "description" => match arg {
                    Some(d) => probe.description = Some(d),
                    None => need("a text"),
                },
                "control" => match arg.as_deref() {
                    Some("native") => probe.control = Control::Native,
                    Some("tui") => probe.control = Control::Tui,
                    _ => need("native or tui"),
                },
                "timeout-secs" => match secs(kdlx::args(node).next().and_then(|v| v.as_integer())) {
                    Some(n) => probe.timeout_secs = n,
                    None => need("a number of seconds"),
                },
                "option" => {
                    let mut it = kdlx::args(node);
                    match (it.next().and_then(|v| v.as_string()), it.next()) {
                        (Some(k), Some(v)) => {
                            probe.options.insert(k.to_string(), kdlx::text(v));
                        }
                        _ => need("an id and a value"),
                    }
                }
                "prompt" => match arg {
                    Some(text) => probe.steps.push(ProbeStep::Prompt { text }),
                    None => need("a text"),
                },
                "wait" => match arg {
                    Some(until) => probe.steps.push(ProbeStep::Wait {
                        until,
                        timeout_secs: secs(kdlx::prop_int(node, "timeout-secs")),
                    }),
                    None => need("a state or request"),
                },
                "answer" => match arg {
                    Some(choice) => probe.steps.push(ProbeStep::Answer { choice }),
                    None => need("a choice id"),
                },
                "cancel" => probe.steps.push(ProbeStep::Cancel),
                "keys" => {
                    let text = kdlx::prop_str(node, "text").map(str::to_string);
                    let press: Vec<String> = kdlx::args(node).map(kdlx::text).collect();
                    if text.is_none() && press.is_empty() {
                        need("text= or keys to press");
                    }
                    probe.steps.push(ProbeStep::Keys { text, press });
                }
                "sleep-ms" => match secs(kdlx::args(node).next().and_then(|v| v.as_integer())) {
                    Some(ms) => probe.steps.push(ProbeStep::Sleep { ms }),
                    None => need("a number of milliseconds"),
                },
                other => problems.push(format!("unknown probe node {other}")),
            }
        }
        if probe.steps.is_empty() && problems.is_empty() {
            problems.push("the probe has no steps".into());
        }
        if problems.is_empty() {
            Ok(probe)
        } else {
            Err(problems)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_steps_in_order() {
        let p = Probe::parse(
            "description \"d\"\ntimeout-secs 30\noption \"model\" \"fast\"\nprompt \"hi\"\nwait \"idle\" timeout-secs=5\nkeys \"Enter\" text=\"/help\"\ncancel\nsleep-ms 10\n",
        )
        .unwrap();
        assert_eq!(p.timeout_secs, 30);
        assert_eq!(p.options["model"], "fast");
        assert_eq!(p.steps.len(), 5);
        assert_eq!(
            p.steps[1],
            ProbeStep::Wait {
                until: "idle".into(),
                timeout_secs: Some(5)
            }
        );
    }

    #[test]
    fn refuses_unknown_nodes() {
        assert!(Probe::parse("prompt \"hi\"\nfly").unwrap_err()[0].contains("fly"));
        assert!(Probe::parse("description \"only\"").is_err());
    }
}
