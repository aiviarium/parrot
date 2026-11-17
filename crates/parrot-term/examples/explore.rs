//! Drives a program from a small script and prints screens, for working out what a TUI shows
//! before writing a recording scenario or a profile.
//!
//! ```text
//! cargo run -p parrot-term --example explore -- SCRIPT [--cast OUT] [--size COLSxROWS] -- CMD...
//! ```
//!
//! Script lines: `wait TEXT`, `waitlong TEXT`, `gone TEXT`, `type TEXT` (`\r`, `\n`, `\t`,
//! `\e` escapes), `key NAME`, `sleep MS`, `dump`, `resize COLSxROWS`, `mark LABEL`, `# comment`.

use std::time::Duration;

use parrot_term::snapshot::to_text;
use parrot_term::{CastHeader, PtyCommand, Session, SessionConfig, Size};

fn unescape(s: &str) -> String {
    s.replace("\\r", "\r")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\e", "\x1b")
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dash = args.iter().position(|a| a == "--").expect("-- CMD");
    let script = std::fs::read_to_string(&args[0]).expect("script");
    let mut size = Size::new(30, 100);
    let mut cast = None;
    let mut i = 1;
    while i < dash {
        match args[i].as_str() {
            "--size" => {
                let (c, r) = args[i + 1].split_once('x').unwrap();
                size = Size::new(r.parse().unwrap(), c.parse().unwrap());
                i += 2;
            }
            "--cast" => {
                cast = Some(args[i + 1].clone());
                i += 2;
            }
            _ => i += 1,
        }
    }
    let cmd_args = &args[dash + 1..];
    let mut cmd = PtyCommand::new(cmd_args[0].clone()).args(cmd_args[1..].iter().cloned());
    if let Ok(cwd) = std::env::var("EXPLORE_CWD") {
        cmd = cmd.cwd(cwd);
    }
    cmd = cmd.env("TERM", "xterm-256color").env("LANG", "en_US.UTF-8");
    let (session, pty) = Session::spawn(&cmd, SessionConfig::with_size(size)).expect("spawn");
    if let Some(path) = &cast {
        let file = std::fs::File::create(path).unwrap();
        session
            .start_recording(std::io::BufWriter::new(file), CastHeader::new(size))
            .unwrap();
    }
    let start = std::time::Instant::now();
    for line in script.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (op, arg) = line.split_once(' ').unwrap_or((line, ""));
        let t = start.elapsed().as_secs_f64();
        let r: Result<(), String> = match op {
            "wait" | "waitlong" => {
                let to = if op == "wait" { 30 } else { 180 };
                session
                    .wait_for_text(arg, Duration::from_secs(to))
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            "gone" => session
                .wait_for(|s| !s.contains(arg), Duration::from_secs(180))
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            "type" => session.type_text(&unescape(arg)).await.map_err(|e| e.to_string()),
            "key" => session.press(arg).await.map_err(|e| e.to_string()),
            "sleep" => {
                tokio::time::sleep(Duration::from_millis(arg.parse().unwrap())).await;
                Ok(())
            }
            "mark" => {
                session.marker(arg);
                Ok(())
            }
            "resize" => {
                let (c, r) = arg.split_once('x').unwrap();
                session
                    .resize(Size::new(r.parse().unwrap(), c.parse().unwrap()))
                    .await
                    .map_err(|e| e.to_string())
            }
            "dump" => {
                println!("── dump at {t:.3}s\n{}", to_text(&session.screen()));
                Ok(())
            }
            _ => Err(format!("unknown op {op}")),
        };
        eprintln!("[{t:7.3}] {line} {}", if r.is_ok() { "ok" } else { "FAILED" });
        if let Err(e) = r {
            eprintln!("{e}");
            println!("── failed at {t:.3}s\n{}", to_text(&session.screen()));
            break;
        }
    }
    if tokio::time::timeout(Duration::from_secs(5), pty.wait())
        .await
        .is_err()
    {
        let _ = pty.kill();
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), session.wait_closed()).await;
    let _ = session.stop_recording();
}
