//! `parrot probe`: runs a probe scenario against a real CLI and records its frames and transcript. Not run in CI.
//!
//! The scenario is `probes/<scenario>.kdl` in the agent's egg. The driver is resolved as
//! `parrot serve` would, and run without a runtime. The output folder gets:
//!
//! | File | Holds |
//! | --- | --- |
//! | `transcript.jsonl` | Every call to the driver, its answer and every update, in order, with `at` in ms from the start |
//! | `frames.jsonl` | The `frame` updates: raw source output, for fixtures |
//! | `probe.json` | The agent, egg, scenario, harness version and outcome |

use crate::serve::default_state_dir;
use parrot_driver::host::ProcessDriver;
use parrot_driver::traits::Driver;
use parrot_eggs::{Probe, ProbeStep};
use parrot_proto::rpc::RpcError;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(clap::Args)]
pub struct Args {
    /// The agent, such as `claude`.
    agent: String,
    /// The scenario: `probes/<scenario>.kdl` in the agent's egg.
    scenario: String,
    /// Where to write the recording.
    #[arg(long)]
    out: PathBuf,
    /// The folder the harness works in; the current one by default.
    #[arg(long)]
    dir: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

/// How long one step waits when the scenario gives no timeout.
const STEP_WAIT: Duration = Duration::from_secs(60);

const BINDING: &str = "bnd_probe";

pub async fn run(args: Args) -> Result<(), String> {
    let state_dir = match args.state_dir {
        Some(d) => d,
        None => default_state_dir().map_err(|e| e.to_string())?,
    };
    let (drivers, _) = parrot_serve::drivers::Drivers::load(&state_dir);
    let egg = drivers
        .eggs()
        .find(|e| e.agent == args.agent)
        .ok_or_else(|| format!("no driver egg for agent {}", args.agent))?
        .clone();
    let path = egg.data_dir.join("probes").join(format!("{}.kdl", args.scenario));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let probe = Probe::parse(&text).map_err(|p| format!("{}: {}", path.display(), p.join("; ")))?;
    let cmd = drivers.resolve(&args.agent)?;
    let cwd = match args.dir {
        Some(d) => d,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    std::fs::create_dir_all(&args.out).map_err(|e| format!("cannot create {}: {e}", args.out.display()))?;

    let (mut driver, updates) = ProcessDriver::spawn(&cmd.program, &cmd.args, cmd.env.clone())
        .map_err(|e| format!("cannot start {}: {e}", cmd.program.display()))?;
    let mut rec = Recorder::new(&args.out, updates)?;
    let deadline = Instant::now() + Duration::from_secs(probe.timeout_secs);
    let outcome = run_steps(&mut driver, &mut rec, &probe, &args.agent, &cwd, deadline).await;
    let _ = rec
        .call(&mut driver, "binding/stop", json!({ "binding": BINDING }))
        .await;
    driver.stop().await;
    rec.drain();

    // The egg's range decides, as in the runtime (spec 31).
    let range = egg
        .driver
        .as_ref()
        .ok()
        .and_then(|d| d.cli.as_ref())
        .and_then(|c| c.versions.clone());
    if let (Some(h), Some(range)) = (rec.harness.as_mut(), range) {
        let version = h["version"].as_str().unwrap_or_default().to_string();
        h["verified"] = json!(parrot_core::agents::covers(&range, &version));
    }
    let summary = json!({
        "agent": args.agent,
        "package": egg.package,
        "version": egg.version,
        "scenario": args.scenario,
        "harness": rec.harness,
        "ok": outcome.is_ok(),
        "error": outcome.as_ref().err(),
        "updates": rec.updates,
        "frames": rec.frames,
    });
    let file = args.out.join("probe.json");
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&summary).unwrap_or_default() + "\n",
    )
    .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    outcome.map(|()| println!("recorded {} updates into {}", rec.updates, args.out.display()))
}

async fn run_steps(
    driver: &mut ProcessDriver,
    rec: &mut Recorder,
    probe: &Probe,
    agent: &str,
    cwd: &Path,
    deadline: Instant,
) -> Result<(), String> {
    let fail = |what: &str, e: RpcError| format!("{what} failed: {} ({})", e.message, e.code);
    rec.call(driver, "initialize", initialize_params())
        .await
        .map_err(|e| fail("initialize", e))?;
    let listed = rec
        .call(driver, "agent/list", json!({}))
        .await
        .map_err(|e| fail("agent/list", e))?;
    rec.harness = listed["agents"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["id"] == agent).or(a.first()))
        .and_then(|a| a.get("harness").cloned());
    let start = json!({
        "session": "ses_probe",
        "binding": BINDING,
        "agent": agent,
        "workspace": cwd,
        "cwd": cwd,
        "options": probe.options,
        "control": probe.control,
        "deny": [],
    });
    rec.call(driver, "binding/start", start)
        .await
        .map_err(|e| fail("binding/start", e))?;
    for (n, step) in probe.steps.iter().enumerate() {
        if Instant::now() > deadline {
            return Err(format!("the scenario ran out of time at step {}", n + 1));
        }
        let (method, params) = match step {
            ProbeStep::Prompt { text } => (
                "binding/prompt",
                json!({ "binding": BINDING, "message": format!("msg_probe_{n}"), "text": text, "steer": false }),
            ),
            ProbeStep::Answer { choice } => {
                let request = rec
                    .last_request
                    .clone()
                    .ok_or_else(|| format!("step {}: no request to answer", n + 1))?;
                (
                    "binding/answer",
                    json!({ "binding": BINDING, "request": request, "answer": { "choice": choice } }),
                )
            }
            ProbeStep::Cancel => ("binding/cancel", json!({ "binding": BINDING })),
            ProbeStep::Keys { text, press } => (
                "binding/keys",
                json!({ "binding": BINDING, "text": text, "press": press }),
            ),
            ProbeStep::Sleep { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                rec.drain();
                continue;
            }
            ProbeStep::Wait { until, timeout_secs } => {
                let wait = timeout_secs.map_or(STEP_WAIT, Duration::from_secs);
                let until_at = (Instant::now() + wait).min(deadline);
                rec.wait(until, until_at)
                    .await
                    .map_err(|()| format!("step {}: no {until} within {}s", n + 1, wait.as_secs()))?;
                continue;
            }
        };
        rec.call(driver, method, params)
            .await
            .map_err(|e| fail(method, e))?;
    }
    Ok(())
}

fn initialize_params() -> Value {
    json!({
        "protocol": [1],
        "peer": { "kind": "runtime", "name": "parrot-probe", "version": env!("CARGO_PKG_VERSION") },
        "features": {},
    })
}

/// Writes the transcript and frames as they happen.
struct Recorder {
    started: Instant,
    transcript: std::fs::File,
    frames_file: std::fs::File,
    rx: mpsc::UnboundedReceiver<Value>,
    harness: Option<Value>,
    last_request: Option<String>,
    /// States and `request` seen since the last call or wait.
    seen: Vec<String>,
    updates: u64,
    frames: u64,
}

impl Recorder {
    fn new(out: &Path, rx: mpsc::UnboundedReceiver<Value>) -> Result<Self, String> {
        let open = |name: &str| {
            let p = out.join(name);
            std::fs::File::create(&p).map_err(|e| format!("cannot create {}: {e}", p.display()))
        };
        Ok(Self {
            started: Instant::now(),
            transcript: open("transcript.jsonl")?,
            frames_file: open("frames.jsonl")?,
            rx,
            harness: None,
            last_request: None,
            seen: Vec::new(),
            updates: 0,
            frames: 0,
        })
    }

    fn line(&mut self, mut v: Value) {
        v["at"] = json!(self.started.elapsed().as_millis() as u64);
        let _ = writeln!(self.transcript, "{v}");
    }

    async fn call(
        &mut self,
        driver: &mut ProcessDriver,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        // A wait after this call looks only at what the call caused.
        self.drain();
        self.seen.clear();
        self.line(json!({ "dir": "out", "method": method, "params": params }));
        let result = driver.call(method, params).await;
        self.drain();
        match &result {
            Ok(r) => self.line(json!({ "dir": "in", "method": method, "result": r })),
            Err(e) => self.line(json!({ "dir": "in", "method": method, "error": e })),
        }
        result
    }

    fn update(&mut self, u: Value) {
        self.updates += 1;
        let kind = u
            .pointer("/update/type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match kind.as_str() {
            "state" => {
                if let Some(s) = u.pointer("/update/state").and_then(Value::as_str) {
                    self.seen.push(s.to_string());
                }
            }
            "request" => {
                self.last_request = u
                    .pointer("/update/request/id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.seen.push("request".into());
            }
            "frame" => {
                self.frames += 1;
                let _ = writeln!(self.frames_file, "{}", u["update"]);
            }
            _ => {}
        }
        self.line(json!({ "dir": "in", "update": u }));
    }

    fn drain(&mut self) {
        while let Ok(u) = self.rx.try_recv() {
            self.update(u);
        }
    }

    /// Waits for a state (or `request`) seen since the last wait.
    async fn wait(&mut self, until: &str, at: Instant) -> Result<(), ()> {
        loop {
            self.drain();
            if let Some(i) = self.seen.iter().position(|s| s == until) {
                self.seen.drain(..=i);
                return Ok(());
            }
            let left = at.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.rx.recv()).await {
                Ok(Some(u)) => self.update(u),
                _ => return Err(()),
            }
        }
    }
}
