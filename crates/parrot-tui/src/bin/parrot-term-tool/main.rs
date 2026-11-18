//! `parrot-term-tool`: record a command into an asciinema cast, and replay casts.
//!
//! ```text
//! parrot-term-tool record [-o FILE] [--size COLSxROWS] -- CMD [ARGS...]
//! parrot-term-tool replay CAST [--at SECONDS] [--styles]
//! parrot-term-tool snapshot CAST [--styles] [--history N]
//! parrot-term-tool markers CAST
//! ```

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use parrot_term::snapshot::{SnapshotOptions, to_text_with};
use parrot_term::{
    Cast, CastHeader, PtyCommand, Replay, ReplayConfig, Session, SessionConfig, SettleConfig, Size,
};
use parrot_tui::{Profile, Tracker};

mod inspect;

const USAGE: &str = "usage:
  parrot-term-tool record [-o FILE] [--size COLSxROWS] [--] CMD [ARGS...]
      Runs CMD on a PTY attached to this terminal and records it (default: rec.cast).
  parrot-term-tool replay CAST [--at SECONDS] [--styles]
      Prints every settled snapshot, or the screen at SECONDS.
  parrot-term-tool snapshot CAST [--styles] [--history N]
      Prints the screen at the end of the cast.
  parrot-term-tool markers CAST
      Lists the markers in the cast.
  parrot-term-tool tree CAST [--at SECONDS] [--profile FILE] [--styles] [--json]
      Prints the tree at SECONDS (default: the end) with bounds, roles, tags and the rule
      that produced each node. With a profile, also the states that hold.
  parrot-term-tool states CAST --profile FILE [--settle default|automation]
      Prints the state timeline of the cast.
  parrot-term-tool grep CAST REGEX
      Prints when a regex over the screen starts and stops matching, output event by event.
  parrot-term-tool measure [--tolerance S] [--settle default|automation] [-v] TRUTH...
      Scores state detection against truth files (or directories of them).
  parrot-term-tool inspect [--profile FILE] [--size COLSxROWS] -- CMD [ARGS...]
      Runs CMD with its screen on the left and its tree and state on the right. The profile
      is reloaded when the file changes. Ctrl+] quits.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("record") => record(&args[1..]),
        Some("replay") => replay(&args[1..]),
        Some("snapshot") => snapshot(&args[1..]),
        Some("markers") => markers(&args[1..]),
        Some("tree") => tree(&args[1..]),
        Some("states") => states(&args[1..]),
        Some("grep") => grep(&args[1..]),
        Some("measure") => measure(&args[1..]),
        Some("inspect") => inspect::run(&args[1..]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn parse_size(s: &str) -> Result<Size, String> {
    let (c, r) = s.split_once('x').ok_or("size must be COLSxROWS")?;
    Ok(Size::new(
        r.parse().map_err(|_| "bad rows")?,
        c.parse().map_err(|_| "bad cols")?,
    ))
}

fn load(path: Option<&String>) -> Result<Cast, String> {
    let path = path.ok_or(USAGE)?;
    Cast::load(path).map_err(|e| format!("{path}: {e}"))
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1))
}

fn replay(args: &[String]) -> Result<ExitCode, String> {
    let cast = load(args.first())?;
    let styles = args.iter().any(|a| a == "--styles");
    let opts = SnapshotOptions {
        styles,
        history: None,
    };
    let replay = Replay::new(&cast);
    let mut out = std::io::stdout().lock();
    if let Some(at) = flag_value(args, "--at") {
        let t: f64 = at.parse().map_err(|_| "--at takes seconds")?;
        let _ = write!(out, "{}", to_text_with(&replay.screen_at(t), opts));
        return Ok(ExitCode::SUCCESS);
    }
    for (i, f) in replay.frames().iter().enumerate() {
        let _ = writeln!(out, "── frame {i} at {:.3}s ({:?}) ──", f.time, f.reason);
        let _ = write!(out, "{}", to_text_with(&f.screen, opts));
    }
    Ok(ExitCode::SUCCESS)
}

fn snapshot(args: &[String]) -> Result<ExitCode, String> {
    let cast = load(args.first())?;
    let history = match flag_value(args, "--history") {
        Some(n) => Some(n.parse().map_err(|_| "--history takes a number")?),
        None => None,
    };
    let opts = SnapshotOptions {
        styles: args.iter().any(|a| a == "--styles"),
        history,
    };
    print!("{}", to_text_with(&Replay::new(&cast).final_screen(), opts));
    Ok(ExitCode::SUCCESS)
}

fn markers(args: &[String]) -> Result<ExitCode, String> {
    let cast = load(args.first())?;
    for (t, label) in cast.markers() {
        println!("{t:>10.3}  {label}");
    }
    println!("{:>10.3}  (end)", cast.duration());
    Ok(ExitCode::SUCCESS)
}

#[cfg(unix)]
pub(crate) fn terminal_size() -> Option<Size> {
    let ws = rustix::termios::tcgetwinsize(std::io::stdout()).ok()?;
    (ws.ws_row > 0 && ws.ws_col > 0).then(|| Size::new(ws.ws_row, ws.ws_col))
}

/// Windows reports no size here; callers fall back to a default.
#[cfg(not(unix))]
pub(crate) fn terminal_size() -> Option<Size> {
    None
}

/// Puts stdin into raw mode until dropped.
#[cfg(unix)]
pub(crate) struct RawMode(Option<rustix::termios::Termios>);

#[cfg(unix)]
impl RawMode {
    pub(crate) fn enable() -> Self {
        let stdin = std::io::stdin();
        match rustix::termios::tcgetattr(&stdin) {
            Ok(orig) => {
                let mut raw = orig.clone();
                raw.make_raw();
                let _ = rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &raw);
                RawMode(Some(orig))
            }
            Err(_) => RawMode(None),
        }
    }
}

#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(orig) = &self.0 {
            let _ = rustix::termios::tcsetattr(std::io::stdin(), rustix::termios::OptionalActions::Now, orig);
        }
    }
}

/// The console on Windows stays as it is; the tool is for reading casts there.
#[cfg(not(unix))]
pub(crate) struct RawMode;

#[cfg(not(unix))]
impl RawMode {
    pub(crate) fn enable() -> Self {
        RawMode
    }
}

fn record(args: &[String]) -> Result<ExitCode, String> {
    let mut out_path = "rec.cast".to_string();
    let mut size = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                out_path = args.get(i + 1).ok_or("-o takes a file")?.clone();
                i += 2;
            }
            "--size" => {
                size = Some(parse_size(args.get(i + 1).ok_or("--size takes COLSxROWS")?)?);
                i += 2;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    let cmd_args = &args[i..];
    let (program, rest) = cmd_args.split_first().ok_or(USAGE)?;
    let size = size.or_else(terminal_size).unwrap_or_default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let mut config = SessionConfig::with_size(size);
        // The real terminal sees the output and answers queries itself.
        config.answer_queries = false;
        let cmd = PtyCommand::new(program.clone()).args(rest.iter().cloned());
        let (session, pty) = Session::spawn(&cmd, config).map_err(|e| format!("{program}: {e}"))?;
        let file = std::fs::File::create(&out_path).map_err(|e| format!("{out_path}: {e}"))?;
        let mut header = CastHeader::new(size);
        header.command = Some(cmd_args.join(" "));
        header.timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        header.env = Some(
            [("TERM".to_string(), "xterm-256color".to_string())]
                .into_iter()
                .chain(std::env::var("SHELL").ok().map(|s| ("SHELL".to_string(), s)))
                .collect(),
        );
        session
            .start_recording(std::io::BufWriter::new(file), header)
            .map_err(|e| e.to_string())?;
        let raw = RawMode::enable();

        let mut bytes = session.subscribe_bytes(Some(0)).map_err(|e| e.to_string())?;
        let printer = tokio::spawn(async move {
            let mut stdout = std::io::stdout();
            while let Ok(Some(c)) = bytes.recv().await {
                let _ = stdout.write_all(&c.data);
                let _ = stdout.flush();
            }
        });

        let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || in_tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            .map_err(|e| e.to_string())?;
        let status = loop {
            tokio::select! {
                status = pty.wait() => break status,
                Some(data) = in_rx.recv() => { let _ = session.write(&data).await; }
                _ = winch.recv() => {
                    if let Some(s) = terminal_size() {
                        let _ = session.resize(s).await;
                    }
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), session.wait_closed()).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), printer).await;
        session.stop_recording().map_err(|e| e.to_string())?;
        drop(raw);
        eprintln!(
            "\r\n[parrot-term-tool] recorded to {out_path} (exit {})",
            status.code
        );
        Ok(ExitCode::from(status.code.min(255) as u8))
    })
}

fn settle_of(args: &[String]) -> Result<SettleConfig, String> {
    match flag_value(args, "--settle").map(String::as_str) {
        None | Some("automation") => Ok(SettleConfig::for_automation()),
        Some("default") => Ok(SettleConfig::default()),
        Some(o) => Err(format!("--settle takes default or automation, not {o}")),
    }
}

fn load_profile(args: &[String]) -> Result<Option<Profile>, String> {
    flag_value(args, "--profile")
        .map(|p| Profile::load(p).map_err(|e| e.to_string()))
        .transpose()
}

/// Replays frames up to `until` through a tracker.
fn track(cast: &Cast, profile: Option<Profile>, settle: SettleConfig, until: Option<f64>) -> Tracker {
    let replay = Replay::with_config(
        cast,
        ReplayConfig {
            settle,
            ..Default::default()
        },
    );
    let mut tracker = Tracker::new(profile, Default::default());
    let frames = replay.frames();
    for f in &frames {
        if until.is_some_and(|u| f.time > u) {
            break;
        }
        tracker.update(f.time, &f.screen);
    }
    if let Some(u) = until
        && frames.first().is_none_or(|f| f.time > u)
    {
        tracker.update(u, &replay.screen_at(u));
    }
    tracker
}

fn tree(args: &[String]) -> Result<ExitCode, String> {
    let cast = load(args.first())?;
    let at = match flag_value(args, "--at") {
        Some(a) => Some(a.parse::<f64>().map_err(|_| "--at takes seconds")?),
        None => None,
    };
    let tracker = track(&cast, load_profile(args)?, settle_of(args)?, at);
    let snap = tracker.snapshot();
    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?
        );
        return Ok(ExitCode::SUCCESS);
    }
    let opts = parrot_tui::tree::RenderOptions {
        styles: args.iter().any(|a| a == "--styles"),
        ..Default::default()
    };
    if snap.profile.is_some() {
        println!(
            "frame at {:.3}s · state {} · holding [{}]{}",
            snap.time,
            snap.state.as_deref().unwrap_or("none"),
            snap.states.join(", "),
            if snap.unrecognised { " · unrecognised" } else { "" }
        );
    }
    if let Some(t) = &snap.tree {
        print!("{}", parrot_tui::tree::render_with(t, opts));
    }
    for s in &snap.surfaces {
        println!(
            "surface {} {:?} {:?}: {} options, selected {:?}{}",
            s.id,
            s.kind,
            s.title.as_deref().unwrap_or(""),
            s.options.len(),
            s.selected,
            if s.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", s.tags.join(","))
            }
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn states(args: &[String]) -> Result<ExitCode, String> {
    let cast = load(args.first())?;
    let profile = load_profile(args)?.ok_or("states needs --profile FILE")?;
    let det = parrot_tui::measure::detect(&cast, &profile, settle_of(args)?);
    let markers = cast.markers();
    let mut mi = 0;
    for (i, (t, s)) in det.changes.iter().enumerate() {
        while mi < markers.len() && markers[mi].0 <= *t {
            println!("{:>10.3}  · marker {}", markers[mi].0, markers[mi].1);
            mi += 1;
        }
        let next = det.changes.get(i + 1).map_or(det.end, |n| n.0);
        println!(
            "{t:>10.3}  {} ({:.3} s)",
            s.as_deref().unwrap_or("none"),
            next - t
        );
    }
    while mi < markers.len() {
        println!("{:>10.3}  · marker {}", markers[mi].0, markers[mi].1);
        mi += 1;
    }
    println!("{:>10.3}  (end, {} frames)", det.end, det.frames);
    Ok(ExitCode::SUCCESS)
}

fn grep(args: &[String]) -> Result<ExitCode, String> {
    let cast = load(args.first())?;
    let re = regex::Regex::new(args.get(1).ok_or("grep CAST REGEX")?).map_err(|e| e.to_string())?;
    let mut emu = parrot_term::Emulator::new(parrot_term::EmulatorConfig::with_size(cast.header.size()));
    let mut was = false;
    for ev in &cast.events {
        match ev.kind {
            parrot_term::EventKind::Output => {
                emu.feed(ev.data.as_bytes());
            }
            parrot_term::EventKind::Resize => {
                if let Some(s) = ev.size() {
                    emu.resize(s);
                }
            }
            parrot_term::EventKind::Marker => {
                println!("{:>10.3}  · marker {}", ev.time, ev.data);
                continue;
            }
            _ => continue,
        }
        let screen = emu.screen();
        let text = screen.lines().join("\n");
        let now = re.is_match(&parrot_term::normalize_spaces(&text));
        if now != was {
            println!("{:>10.3}  {}", ev.time, if now { "appears" } else { "gone" });
            was = now;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn truth_files(paths: &[String]) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        let p = std::path::PathBuf::from(p);
        if p.is_dir() {
            let mut v: Vec<_> = std::fs::read_dir(&p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|e| e.to_string_lossy().ends_with(".truth.toml"))
                .collect();
            v.sort();
            out.extend(v);
        } else {
            out.push(p);
        }
    }
    out
}

fn measure(args: &[String]) -> Result<ExitCode, String> {
    let mut tol = parrot_tui::measure::DEFAULT_TOLERANCE;
    let mut files = Vec::new();
    let mut verbose = false;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tolerance" => {
                tol = args
                    .get(i + 1)
                    .ok_or("--tolerance S")?
                    .parse()
                    .map_err(|_| "bad tolerance")?;
                i += 2;
            }
            "--settle" => i += 2,
            "-v" | "--verbose" => {
                verbose = true;
                i += 1;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            _ => {
                files.push(args[i].clone());
                i += 1;
            }
        }
    }
    let settle = settle_of(args)?;
    let mut groups: std::collections::BTreeMap<(String, bool), parrot_tui::measure::Summary> =
        Default::default();
    let mut reports = Vec::new();
    for f in truth_files(&files) {
        let r = parrot_tui::measure::measure_file(&f, settle, tol)?;
        if verbose && !json {
            print!("{}", r.to_text());
        }
        groups.entry((r.profile.clone(), r.holdout)).or_default().add(&r);
        reports.push(r);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())?
        );
        return Ok(ExitCode::SUCCESS);
    }
    for ((profile, holdout), s) in &groups {
        let title = format!(
            "{profile} ({})",
            if *holdout { "held out" } else { "development" }
        );
        print!("{}", s.to_text(&title));
    }
    Ok(ExitCode::SUCCESS)
}
