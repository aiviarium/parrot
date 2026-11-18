//! `parrot conform`: runs the golden fixtures against a driver or a runtime (spec 38).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use parrot_conform::target::{self, Process};
use parrot_conform::{Bindings, Meta, Report, Step, Target};
use serde_json::Value;

#[derive(clap::Args)]
pub struct Args {
    /// Run the client fixtures against a runtime instead of the driver fixtures against a driver.
    #[arg(long)]
    pub runtime: bool,
    /// The runtime's socket; without it, each fixture starts its own `parrot serve`.
    #[arg(long, requires = "runtime")]
    pub socket: Option<PathBuf>,
    /// A directory of `.jsonl` fixtures to run instead of the built-in ones.
    #[arg(long)]
    pub fixtures: Option<PathBuf>,
    /// A directory of harness scenarios, `<scenario>.jsonl`, for the target's fake harness.
    #[arg(long, requires = "scenario_var")]
    pub scenarios: Option<PathBuf>,
    /// The variable that tells the fake harness which scenario file to play.
    #[arg(long, requires = "scenarios")]
    pub scenario_var: Option<String>,
    /// The agent id the fixtures start bindings and sessions with.
    #[arg(long, default_value = "claude")]
    pub agent: String,
    /// Run only the fixtures with these names.
    #[arg(long)]
    pub only: Vec<String>,
    /// Also run fixtures marked pending.
    #[arg(long)]
    pub pending: bool,
    /// Seconds to wait for each message.
    #[arg(long, default_value_t = 10)]
    pub wait: u64,
    /// Print every message received by a failing fixture.
    #[arg(long, short)]
    pub verbose: bool,
    /// The driver command to test, such as `parrot driver claude`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

/// A temporary directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("parrot-conform-{}-{nanos}-{name}", std::process::id()));
        std::fs::create_dir_all(dir.join("work"))?;
        Ok(Scratch(dir))
    }

    fn work(&self) -> PathBuf {
        // Harnesses report canonical paths, so fixtures compare against the canonical one.
        let work = self.0.join("work");
        std::fs::canonicalize(&work).unwrap_or(work)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn run(args: Args) -> i32 {
    if !args.runtime && args.command.is_empty() {
        eprintln!(
            "parrot conform: give a driver command, such as `parrot conform parrot driver claude`, or --runtime"
        );
        return 2;
    }
    let fixtures = match load(&args) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("parrot conform: {e}");
            return 2;
        }
    };
    let suite = if args.runtime { "runtime" } else { "driver" };
    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for (name, steps) in &fixtures {
        let meta = parrot_conform::meta(steps);
        if let Some(why) = meta.pending.as_ref().filter(|_| !args.pending) {
            println!("skip  {suite}/{name}: pending: {why}");
            skipped += 1;
            continue;
        }
        let started = Instant::now();
        match run_one(&args, name, &meta, steps) {
            Ok(report) if report.passed() => {
                println!(
                    "pass  {suite}/{name} ({} steps, {:.1?})",
                    report.steps,
                    started.elapsed()
                );
                passed += 1;
            }
            Ok(report) => {
                for m in &report.mismatches {
                    println!("FAIL  {suite}/{name}: line {}: {}: {}", m.line, m.path, m.reason);
                }
                if args.verbose {
                    for msg in &report.received {
                        println!("      < {msg}");
                    }
                }
                failed += 1;
            }
            Err(e) => {
                println!("FAIL  {suite}/{name}: {e}");
                failed += 1;
            }
        }
    }
    println!("{passed} passed, {failed} failed, {skipped} skipped");
    if failed > 0 { 1 } else { 0 }
}

fn load(args: &Args) -> Result<Vec<(String, Vec<Step>)>, String> {
    let mut texts: Vec<(String, String)> = match &args.fixtures {
        Some(dir) => {
            let mut files: Vec<_> = std::fs::read_dir(dir)
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            files.sort();
            files
                .into_iter()
                .map(|p| {
                    let name = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    std::fs::read_to_string(&p)
                        .map(|t| (name, t))
                        .map_err(|e| format!("cannot read {}: {e}", p.display()))
                })
                .collect::<Result<_, _>>()?
        }
        None => {
            let builtin = if args.runtime {
                parrot_conform::fixtures::RUNTIME
            } else {
                parrot_conform::fixtures::DRIVER
            };
            builtin
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect()
        }
    };
    if !args.only.is_empty() {
        texts.retain(|(n, _)| args.only.contains(n));
    }
    texts
        .into_iter()
        .map(|(name, text)| {
            parrot_conform::parse(&text)
                .map(|steps| (name.clone(), steps))
                .map_err(|e| format!("{name}: {e}"))
        })
        .collect()
}

/// The environment that selects the fixture's scenario in the target's fake harness.
fn scenario_env(args: &Args, meta: &Meta) -> Result<Vec<(OsString, OsString)>, String> {
    let Some(scenario) = &meta.scenario else {
        return Ok(vec![]);
    };
    let mut env = vec![("PARROT_CONFORM_SCENARIO".into(), scenario.into())];
    if let (Some(dir), Some(var)) = (&args.scenarios, &args.scenario_var) {
        let file = dir.join(format!("{scenario}.jsonl"));
        if !file.exists() {
            return Err(format!("no scenario {}", file.display()));
        }
        let file = std::fs::canonicalize(&file).unwrap_or(file);
        env.push((var.into(), file.into()));
    }
    Ok(env)
}

fn run_one(args: &Args, name: &str, meta: &Meta, steps: &[Step]) -> Result<Report, String> {
    let scratch = Scratch::new(name).map_err(|e| format!("cannot make a scratch directory: {e}"))?;
    let env = scenario_env(args, meta)?;
    let bindings = Bindings::with_vars([
        (
            "dir".into(),
            Value::String(scratch.work().to_string_lossy().into_owned()),
        ),
        ("agent".into(), Value::String(args.agent.clone())),
    ]);
    let wait = Duration::from_secs(args.wait);
    if !args.runtime {
        let mut driver = Process::spawn(&args.command, &env)
            .map_err(|e| format!("cannot start {}: {e}", args.command.join(" ")))?;
        driver.set_wait(wait);
        let report = parrot_conform::run_with(&mut driver, steps, bindings);
        driver.finish(Duration::from_secs(5));
        return Ok(report);
    }
    runtime(args, &scratch, &env, wait, |target| {
        parrot_conform::run_with(target, steps, bindings)
    })
}

/// A `parrot serve` started for one fixture, killed on drop.
struct Serve(Child);

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
fn runtime(
    args: &Args,
    scratch: &Scratch,
    env: &[(OsString, OsString)],
    wait: Duration,
    go: impl FnOnce(&mut dyn Target) -> Report,
) -> Result<Report, String> {
    let (_serve, socket) = match &args.socket {
        Some(socket) => (None, socket.clone()),
        None => {
            let socket = scratch.0.join("sock");
            (Some(start_serve(&scratch.0, &socket, env)?), socket)
        }
    };
    let mut conn =
        target::socket(&socket).map_err(|e| format!("cannot connect to {}: {e}", socket.display()))?;
    conn.wait = wait;
    Ok(go(&mut conn))
}

#[cfg(not(unix))]
fn runtime(
    _: &Args,
    _: &Scratch,
    _: &[(OsString, OsString)],
    _: Duration,
    _: impl FnOnce(&mut dyn Target) -> Report,
) -> Result<Report, String> {
    Err("conform --runtime needs a Unix socket".into())
}

#[cfg(unix)]
fn start_serve(dir: &Path, socket: &Path, env: &[(OsString, OsString)]) -> Result<Serve, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find parrot: {e}"))?;
    let child = Command::new(exe)
        .args(["serve", "--state-dir"])
        .arg(dir.join("state"))
        .env(crate::serve::SOCKET_ENV, socket)
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start parrot serve: {e}"))?;
    let serve = Serve(child);
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        if Instant::now() > deadline {
            return Err("parrot serve never listened".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(serve)
}
