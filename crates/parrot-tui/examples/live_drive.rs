//! Drives a real agent CLI end to end through its profile's actions only, several times, and
//! reports the success rate and every failure with its cause.
//!
//! ```text
//! cargo run -p parrot-tui --example live_drive -- claude|codex [--runs N] [--work DIR]
//! ```
//!
//! Each run starts the CLI in a new folder under `--work`, answers its first-run dialogs,
//! waits for idle, submits a prompt and waits for working then idle, provokes an approval and
//! approves it, provokes another and denies it, interrupts a running turn, and quits. Every
//! run is recorded as `run-N.cast` in its folder, and a failure's tree goes to
//! `run-N.failure.txt`. Uses the cheapest model and trivial prompts.

use parrot_tui::SessionExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use parrot_term::{CastHeader, PtyCommand, Session, SessionConfig, Size};
use parrot_tui::{Automation, Profile, TuiError};

struct Cli {
    name: &'static str,
    /// The egg whose `parrot/profile.kdl` is the live profile.
    profile: &'static str,
    cmd: &'static [&'static str],
    approve: &'static str,
    deny: &'static str,
}

const CLAUDE: Cli = Cli {
    name: "claude",
    profile: "claude",
    cmd: &[
        "claude",
        "--model",
        "haiku",
        "--permission-mode",
        "manual",
        "--setting-sources",
        "project,local",
    ],
    approve: "Use the Bash tool to run exactly this command: echo live > live.txt",
    deny: "Use the Bash tool to run exactly this command: rm live.txt",
};

const CODEX: Cli = Cli {
    name: "codex",
    profile: "codex",
    cmd: &[
        "codex",
        "--no-daemon",
        "-m",
        "gpt-6-luna",
        "-a",
        "on-request",
        "-s",
        "read-only",
    ],
    approve: "Use a shell command to create a file live.txt containing the word live. If the sandbox blocks it, request approval to run it outside the sandbox.",
    deny: "Use a shell command to delete live.txt. If the sandbox blocks it, request approval to run it outside the sandbox.",
};

fn env() -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    for key in [
        "HOME",
        "PATH",
        "USER",
        "LOGNAME",
        "SHELL",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.push((key, v));
        }
    }
    env.push(("LANG", "en_US.UTF-8".into()));
    env.push(("TERM", "xterm-256color".into()));
    env
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const SHORT: Duration = Duration::from_secs(30);
const LONG: Duration = Duration::from_secs(180);

/// A failed step: what was being done and the error.
struct Failure {
    step: &'static str,
    error: TuiError,
}

async fn step<T>(
    log: &mut Vec<String>,
    name: &'static str,
    started: Instant,
    f: impl std::future::Future<Output = Result<T, TuiError>>,
) -> Result<T, Failure> {
    let t = Instant::now();
    let r = f.await;
    log.push(format!(
        "{:>7.2}s {name} {} ({} ms)",
        started.elapsed().as_secs_f64(),
        if r.is_ok() { "ok" } else { "FAILED" },
        t.elapsed().as_millis()
    ));
    r.map_err(|error| Failure { step: name, error })
}

async fn wait_any(tui: &Automation, states: &[&str], timeout: Duration) -> Result<String, TuiError> {
    let cond = parrot_tui::profile::Cond {
        state: Some(parrot_tui::profile::OneOrMany::Many(
            states.iter().map(|s| s.to_string()).collect(),
        )),
        // A state seen for a frame or two (a redraw between a dialog and the prompt) is not
        // the state to act on.
        hold: Some(parrot_tui::profile::Dur(Duration::from_millis(500))),
        ..Default::default()
    };
    tui.wait_for(&cond, timeout)
        .await
        .map(|s| s.state.unwrap_or_default())
}

async fn run_once(
    cli: &Cli,
    profile: &Profile,
    work: &PathBuf,
    n: usize,
) -> Result<Vec<String>, (Vec<String>, Failure)> {
    let started = Instant::now();
    let mut log = Vec::new();
    let mut cmd = PtyCommand::new(cli.cmd[0])
        .args(cli.cmd[1..].iter().copied())
        .cwd(work)
        .env_clear();
    for (k, v) in env() {
        cmd = cmd.env(k, v);
    }
    let size = Size::new(30, 100);
    let (session, pty) = Session::spawn(&cmd, SessionConfig::for_automation(size)).expect("spawn");
    let file = std::fs::File::create(work.join(format!("run-{n}.cast"))).unwrap();
    let mut header = CastHeader::new(size);
    header.title = Some(format!("{} live run {n}", cli.name));
    let _ = session.start_recording(std::io::BufWriter::new(file), header);
    let tui = session.automate(Some(profile.clone()));
    let none = HashMap::new();
    let result: Result<(), Failure> = async {
        // First-run dialogs, answered through the profile's actions.
        loop {
            let s = step(
                &mut log,
                "wait for onboarding or idle",
                started,
                wait_any(&tui, &["onboarding", "idle"], SHORT),
            )
            .await?;
            if s == "idle" {
                break;
            }
            let snap = tui.snapshot();
            let tree = snap.tree.as_ref().unwrap();
            let action = if tree.find_tag("trust-dialog").is_some() {
                "trust"
            } else {
                "skip-update"
            };
            step(
                &mut log,
                if action == "trust" { "trust" } else { "skip-update" },
                started,
                tui.act(action, &none),
            )
            .await?;
        }
        let v = vars(&[("text", "Reply with the single word: pong")]);
        step(&mut log, "submit", started, tui.act("submit", &v)).await?;
        step(
            &mut log,
            "wait for working",
            started,
            wait_any(&tui, &["working"], SHORT),
        )
        .await?;
        step(
            &mut log,
            "wait for idle",
            started,
            wait_any(&tui, &["idle"], LONG),
        )
        .await?;

        step(
            &mut log,
            "submit (to approve)",
            started,
            tui.act("submit", &vars(&[("text", cli.approve)])),
        )
        .await?;
        step(
            &mut log,
            "wait for approval",
            started,
            wait_any(&tui, &["awaiting-approval"], LONG),
        )
        .await?;
        step(&mut log, "approve", started, tui.act("approve", &none)).await?;
        step(
            &mut log,
            "wait for idle",
            started,
            wait_any(&tui, &["idle"], LONG),
        )
        .await?;

        step(
            &mut log,
            "submit (to deny)",
            started,
            tui.act("submit", &vars(&[("text", cli.deny)])),
        )
        .await?;
        step(
            &mut log,
            "wait for approval",
            started,
            wait_any(&tui, &["awaiting-approval"], LONG),
        )
        .await?;
        step(&mut log, "deny", started, tui.act("deny", &none)).await?;
        step(
            &mut log,
            "wait for idle",
            started,
            wait_any(&tui, &["idle"], LONG),
        )
        .await?;

        let v = vars(&[("text", "Write a 60 line poem about the sea.")]);
        step(&mut log, "submit (to interrupt)", started, tui.act("submit", &v)).await?;
        step(
            &mut log,
            "wait for working",
            started,
            wait_any(&tui, &["working"], SHORT),
        )
        .await?;
        step(&mut log, "interrupt", started, tui.act("interrupt", &none)).await?;
        step(
            &mut log,
            "wait for idle",
            started,
            wait_any(&tui, &["idle"], SHORT),
        )
        .await?;
        // Claude Code puts an interrupted prompt back into the input.
        let empty = parrot_tui::profile::Cond {
            empty: Some("prompt".into()),
            ..Default::default()
        };
        if tui.wait_for(&empty, Duration::from_millis(500)).await.is_err() {
            step(&mut log, "clear-prompt", started, tui.act("clear-prompt", &none)).await?;
        }
        step(&mut log, "quit", started, tui.act("quit", &none)).await?;
        Ok(())
    }
    .await;
    let exited = tokio::time::timeout(Duration::from_secs(15), pty.wait()).await;
    if exited.is_err() {
        let _ = pty.kill();
        log.push("did not exit after quit; killed".into());
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), session.wait_closed()).await;
    let _ = session.stop_recording();
    match result {
        Ok(()) if exited.is_ok() => Ok(log),
        Ok(()) => Err((
            log,
            Failure {
                step: "exit",
                error: TuiError {
                    kind: parrot_tui::TuiErrorKind::Timeout,
                    action: Some("quit".into()),
                    step: None,
                    message: "the program did not exit".into(),
                    snapshot: Box::new(tui.snapshot()),
                },
            },
        )),
        Err(f) => Err((log, f)),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match args.first().map(String::as_str) {
        Some("claude") => CLAUDE,
        Some("codex") => CODEX,
        _ => {
            eprintln!("live_drive claude|codex [--runs N] [--work DIR]");
            std::process::exit(2);
        }
    };
    let flag = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1));
    let runs: usize = flag("--runs").map_or(1, |n| n.parse().unwrap());
    let work = PathBuf::from(flag("--work").cloned().unwrap_or_else(|| {
        std::env::temp_dir()
            .join("parrot-live")
            .to_string_lossy()
            .to_string()
    }));
    // The live profiles are the eggs', not the frozen copies in `fixtures/profiles`.
    let profile = Profile::load(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../eggs")
            .join(cli.profile)
            .join("parrot/profile.kdl"),
    )
    .expect("profile");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut ok = 0;
    for n in 1..=runs {
        let dir = work.join(format!("{}-{stamp}-{n}", cli.name));
        std::fs::create_dir_all(&dir).unwrap();
        let t = Instant::now();
        match run_once(&cli, &profile, &dir, n).await {
            Ok(log) => {
                ok += 1;
                println!("run {n}: ok in {:.1}s", t.elapsed().as_secs_f64());
                for l in log {
                    println!("    {l}");
                }
            }
            Err((log, f)) => {
                println!(
                    "run {n}: FAILED at {} after {:.1}s: {}",
                    f.step,
                    t.elapsed().as_secs_f64(),
                    f.error.to_string().lines().next().unwrap_or("")
                );
                for l in log {
                    println!("    {l}");
                }
                let path = dir.join(format!("run-{n}.failure.txt"));
                let _ = std::fs::write(&path, format!("step {}\n{}", f.step, f.error));
                println!("    tree: {}", path.display());
            }
        }
    }
    println!("{}: {ok}/{runs} runs succeeded", cli.name);
}
