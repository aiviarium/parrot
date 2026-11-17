//! Records the casts in `fixtures/casts/` by driving programs through a PTY with waits on the
//! screen.
//!
//! ```text
//! cargo run -p parrot-term --example record_casts -- [--out DIR] [--work DIR] [--dump] NAME...
//! cargo run -p parrot-term --example record_casts -- --list
//! ```
//!
//! `--work` is where throwaway directories for the programs are made (default: a directory in
//! the system temp dir). `--dump` prints every settled frame while recording. With no names,
//! nothing is recorded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use parrot_term::snapshot::to_text;
use parrot_term::{CastHeader, PtyCommand, Session, SessionConfig, Size};
use regex::Regex;

#[derive(Clone, Debug)]
enum Step {
    /// Wait for text on one row.
    Wait(&'static str),
    /// Wait for a regex over the screen.
    WaitRe(&'static str),
    /// Wait until the text is gone.
    WaitGone(&'static str),
    /// Wait for text, with a long timeout (model responses).
    WaitLong(&'static str),
    /// Wait until the regex is gone, with a long timeout.
    WaitGoneLong(&'static str),
    /// Type text.
    Type(&'static str),
    /// Press a key by name.
    Key(&'static str),
    /// Add a marker.
    Mark(&'static str),
    /// Wait until output is quiet for this many milliseconds.
    Stable(u64),
    /// Let time pass, recording whatever the program does (spinners, clocks).
    Idle(u64),
    Resize(u16, u16),
    /// If the text shows up within the time (ms), press the key: for dialogs that only
    /// sometimes appear.
    IfSeen(&'static str, u64, &'static [&'static str]),
    /// Print the screen, while writing a scenario.
    #[allow(dead_code)]
    Dump,
}

use Step::*;

struct Scenario {
    name: &'static str,
    about: &'static str,
    size: Size,
    /// Program and arguments; `{work}` is replaced by the scenario's work directory.
    cmd: &'static [&'static str],
    /// Files to create in the work directory first.
    files: &'static [(&'static str, &'static str)],
    /// Use a fresh work directory even if one exists.
    fresh: bool,
    /// Share the work directory with other scenarios of this group.
    group: Option<&'static str>,
    /// Use a new, uniquely named work directory.
    unique: bool,
    steps: Vec<Step>,
    /// Seconds to wait for the program to exit after the last step; it is killed after that.
    exit_wait: u64,
}

const SHELL: &[&str] = &["bash", "--noprofile", "--norc", "-i"];
/// Claude Code with a cheap model, permission prompts on, and no user-level settings or memory.
const CLAUDE: &[&str] = &[
    "claude",
    "--model",
    "haiku",
    "--permission-mode",
    "manual",
    "--setting-sources",
    "project,local",
];

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "shell-unicode",
            about: "bash with accented text, combining marks, CJK, emoji, ZWJ sequences and a line that wraps on a wide character",
            size: Size::new(20, 60),
            cmd: SHELL,
            files: &[],
            fresh: true,
            group: None,
            unique: false,
            steps: vec![
                Wait("$ "),
                Mark("prompt"),
                Type("echo 'café naïve résumé'\r"),
                WaitRe(r"(?m)^café naïve résumé$"),
                Type("printf 'e\\u0301 a\\u0308 n\\u0303 o\\u0302\\u0323 x\\u20d7\\n'\r"),
                WaitRe(r"(?m)^e\x{301} a\x{308}"),
                Type("echo '界 世界 日本語 한국어 ｆｕｌｌ'\r"),
                WaitRe(r"(?m)^界 世界"),
                Type("echo '🎉 👍🏽 ❤️ ☀️ 👨‍👩‍👧 🇩🇪'\r"),
                WaitRe(r"(?m)^🎉"),
                Stable(200),
                Type("printf 'x%.0s' $(seq 1 59); echo '界 wraps'\r"),
                WaitRe(r"(?m)^界 wraps"),
                Stable(200),
                Mark("unicode-shown"),
                Type(
                    "printf '\\033[1mbold\\033[0m \\033[2mdim\\033[0m \\033[3mitalic\\033[0m \\033[4munder\\033[0m \\033[7mreverse\\033[0m \\033[9mstrike\\033[0m \\033[31mred\\033[0m \\033[38;5;208morange\\033[0m \\033[38;2;10;200;120mrgb\\033[0m\\n'\r",
                ),
                WaitRe(r"(?m)^bold dim"),
                Stable(200),
                Mark("styles-shown"),
                Type("exit\r"),
            ],
            exit_wait: 5,
        },
        Scenario {
            name: "burst-5000",
            about: "a program printing 5000 numbered lines at once, then a prompt",
            size: Size::new(24, 80),
            cmd: SHELL,
            files: &[],
            fresh: true,
            group: None,
            unique: false,
            steps: vec![
                Wait("$ "),
                Type("seq -f 'line %g of 5000' 1 5000; echo done\r"),
                WaitRe(r"(?m)^done$"),
                Stable(300),
                Mark("burst-done"),
                Type("exit\r"),
            ],
            exit_wait: 5,
        },
        Scenario {
            name: "vim",
            about: "vim without a vimrc: insert text with wide characters, a split, a command line, save and quit",
            size: Size::new(24, 80),
            cmd: &["vim", "-u", "NONE", "-N", "-i", "NONE", "notes.txt"],
            files: &[],
            fresh: true,
            group: None,
            unique: false,
            steps: vec![
                Wait("\"notes.txt\""),
                Stable(300),
                Mark("vim-empty"),
                Key("i"),
                Wait("-- INSERT --"),
                Type("Hello from vim\rwide: 界 世界\rcombining: e\u{301} n\u{303}\r"),
                Key("esc"),
                WaitGone("-- INSERT --"),
                Mark("vim-text"),
                Type(":set number\r"),
                Wait("  1 Hello"),
                Type(":split\r"),
                Stable(400),
                Mark("vim-split"),
                Type(":"),
                Stable(200),
                Mark("vim-command-line"),
                Type("wqa\r"),
            ],
            exit_wait: 5,
        },
        Scenario {
            name: "less",
            about: "less on a 300-line file: first page, page down, search, help screen, quit",
            size: Size::new(24, 80),
            cmd: &["less", "data.txt"],
            files: &[("data.txt", "")],
            fresh: true,
            group: None,
            unique: false,
            steps: vec![
                Wait("row 1 "),
                Stable(200),
                Mark("less-first-page"),
                Key("space"),
                Wait("row 30 "),
                Stable(200),
                Mark("less-page-2"),
                Type("/row 150"),
                Stable(200),
                Key("enter"),
                Wait("row 160 "),
                Stable(200),
                Mark("less-search"),
                Key("h"),
                Wait("SUMMARY OF LESS COMMANDS"),
                Stable(200),
                Mark("less-help"),
                Key("q"),
                Stable(200),
                Key("q"),
            ],
            exit_wait: 5,
        },
        Scenario {
            name: "htop",
            about: "htop updating for a few seconds, the setup screen (F2), quit",
            size: Size::new(30, 100),
            cmd: &["htop", "-d", "10"],
            files: &[],
            fresh: true,
            group: None,
            unique: false,
            steps: vec![
                Wait("F10Quit"),
                Stable(300),
                Mark("htop-main"),
                Idle(3000),
                Key("f2"),
                Wait("Setup"),
                Stable(300),
                Mark("htop-setup"),
                Key("esc"),
                Wait("F10Quit"),
                Key("q"),
            ],
            exit_wait: 5,
        },
        Scenario {
            name: "top",
            about: "top refreshing every second for a few seconds, then quit",
            size: Size::new(24, 100),
            cmd: &["top", "-d", "1"],
            files: &[],
            fresh: true,
            group: None,
            unique: false,
            steps: vec![Wait("PID"), Stable(300), Mark("top-main"), Idle(3000), Key("q")],
            exit_wait: 5,
        },
        Scenario {
            name: "nano",
            about: "nano without an rc file: type text, save with ^O (filename prompt), exit with ^X",
            size: Size::new(24, 80),
            cmd: &["nano", "--ignorercfile", "memo.txt"],
            files: &[],
            fresh: true,
            group: None,
            unique: false,
            steps: vec![
                Wait("^X Exit"),
                Stable(300),
                Mark("nano-empty"),
                Type("Hello from nano\rwide: 界\r"),
                Stable(300),
                Mark("nano-text"),
                Key("ctrl+o"),
                Wait("Write to File"),
                Stable(200),
                Mark("nano-save-prompt"),
                Key("enter"),
                Wait("Wrote"),
                Key("ctrl+x"),
            ],
            exit_wait: 5,
        },
    ]
}

fn claude_env() -> Vec<(&'static str, String)> {
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
    env
}

struct Args {
    out: PathBuf,
    work: PathBuf,
    dump: bool,
    names: Vec<String>,
    list: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        out: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/casts"),
        work: std::env::temp_dir().join("parrot-term-casts"),
        dump: false,
        names: Vec::new(),
        list: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => args.out = PathBuf::from(it.next().expect("--out DIR")),
            "--work" => args.work = PathBuf::from(it.next().expect("--work DIR")),
            "--dump" => args.dump = true,
            "--list" => args.list = true,
            _ => args.names.push(a),
        }
    }
    args
}

fn data_file() -> String {
    (1..=300)
        .map(|i| format!("row {i} of the less test file {}\n", "·".repeat(i % 40)))
        .collect()
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let mut all = scenarios();
    all.extend(agent_scenarios());
    all.extend(more_agent_scenarios());
    if args.list || args.names.is_empty() {
        for s in &all {
            println!("{:<22} {}", s.name, s.about);
        }
        return;
    }
    std::fs::create_dir_all(&args.out).unwrap();
    for name in &args.names {
        let Some(sc) = all.iter().find(|s| s.name == name) else {
            eprintln!("unknown scenario {name}");
            std::process::exit(2);
        };
        if let Err(e) = record(sc, &args).await {
            eprintln!("{name}: FAILED: {e}");
            std::process::exit(1);
        }
    }
}

async fn record(sc: &Scenario, args: &Args) -> Result<(), String> {
    let mut work = args.work.join(sc.group.unwrap_or(sc.name));
    if sc.unique {
        // A path the program has never seen, for first-run dialogs keyed by path.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        work = args.work.join(format!("{}-{stamp}", sc.name));
    }
    if sc.fresh && work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    for (file, content) in sc.files {
        let content = if *file == "data.txt" {
            data_file()
        } else {
            content.to_string()
        };
        std::fs::write(work.join(file), content).map_err(|e| e.to_string())?;
    }
    let work_s = work.to_string_lossy().to_string();
    let argv: Vec<String> = sc.cmd.iter().map(|a| a.replace("{work}", &work_s)).collect();
    let mut cmd = PtyCommand::new(argv[0].clone())
        .args(argv[1..].iter().cloned())
        .cwd(&work)
        .env_clear();
    for (k, v) in claude_env() {
        cmd = cmd.env(k, v);
    }
    cmd = cmd.env("PS1", "$ ").env("HISTFILE", "/dev/null");

    let mut config = SessionConfig::with_size(sc.size);
    config.settle.quiet = Duration::from_millis(100);
    let (session, pty) = Session::spawn(&cmd, config).map_err(|e| format!("spawn: {e}"))?;
    let path = args.out.join(format!("{}.cast", sc.name));
    let mut header = CastHeader::new(sc.size);
    header.title = Some(sc.name.to_string());
    header.command = Some(
        argv.iter()
            .map(|a| a.replace(&work_s, "$WORK"))
            .collect::<Vec<_>>()
            .join(" "),
    );
    header.env = Some(BTreeMap::from([("TERM".into(), "xterm-256color".into())]));
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    session
        .start_recording(std::io::BufWriter::new(file), header)
        .map_err(|e| e.to_string())?;

    let dumper = if args.dump {
        let mut frames = session.settled();
        Some(tokio::spawn(async move {
            while let Some(f) = frames.next().await {
                eprintln!("── {:.3}s {:?}\n{}", f.time, f.reason, to_text(&f.screen));
            }
        }))
    } else {
        None
    };

    let result = run_steps(&session, &sc.steps, args.dump, &work).await;
    if result.is_err() {
        session.marker("failed");
    }
    let exited = tokio::time::timeout(Duration::from_secs(sc.exit_wait), pty.wait()).await;
    if exited.is_err() {
        let _ = pty.kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), pty.wait()).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), session.wait_closed()).await;
    session.stop_recording().map_err(|e| e.to_string())?;
    if let Some(d) = dumper {
        d.abort();
    }
    result?;
    eprintln!("{}: recorded {}", sc.name, path.display());
    Ok(())
}

async fn run_steps(session: &Session, steps: &[Step], dump: bool, _work: &Path) -> Result<(), String> {
    const SHORT: Duration = Duration::from_secs(30);
    const LONG: Duration = Duration::from_secs(180);
    for step in steps {
        let r: Result<(), String> = match step {
            Wait(t) => session
                .wait_for_text(*t, SHORT)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            WaitLong(t) => session
                .wait_for_text(*t, LONG)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            WaitRe(re) => session
                .wait_for_text(Regex::new(re).unwrap(), SHORT)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            WaitGone(t) => session
                .wait_for(|s| !s.contains(t), SHORT)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            WaitGoneLong(re) => {
                let re = Regex::new(re).unwrap();
                session
                    .wait_for(|s| !re.is_match(&s.lines().join("\n")), LONG)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            Type(t) => session.type_text(t).await.map_err(|e| e.to_string()),
            Key(k) => session.press(k).await.map_err(|e| e.to_string()),
            Mark(m) => {
                session.marker(m);
                Ok(())
            }
            Stable(ms) => session
                .wait_for_stable(Duration::from_millis(*ms), SHORT)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Idle(ms) => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                Ok(())
            }
            Resize(c, r) => session.resize(Size::new(*r, *c)).await.map_err(|e| e.to_string()),
            IfSeen(t, ms, keys) => {
                if session
                    .wait_for_text(*t, Duration::from_millis(*ms))
                    .await
                    .is_ok()
                {
                    session.marker(&format!("dialog: {t}"));
                    for key in *keys {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        session.press(key).await.map_err(|e| e.to_string())?;
                    }
                }
                Ok(())
            }
            Dump => {
                eprintln!("{}", to_text(&session.screen()));
                Ok(())
            }
        };
        if dump {
            eprintln!("step {step:?}: {}", if r.is_ok() { "ok" } else { "FAILED" });
        }
        r.map_err(|e| format!("step {step:?}: {e}"))?;
    }
    Ok(())
}

/// Steps that quit Claude Code from an empty prompt.
fn claude_exit() -> Vec<Step> {
    vec![
        Type("/exit"),
        Wait("❯ /exit"),
        Idle(800),
        Mark("exit-typed"),
        Key("enter"),
        WaitGone("manual mode on"),
    ]
}

/// Steps that wait for Claude Code's idle prompt after startup, accepting the trust dialog
/// if this folder is new to it.
fn claude_ready() -> Vec<Step> {
    vec![
        IfSeen("Yes, I trust this folder", 4000, &["down", "enter"]),
        Wait("manual mode on"),
        Idle(2500),
        Mark("idle"),
    ]
}

fn claude(name: &'static str, about: &'static str, unique: bool, steps: Vec<Step>) -> Scenario {
    Scenario {
        name,
        about,
        size: Size::new(30, 100),
        cmd: CLAUDE,
        files: &[],
        fresh: false,
        group: Some("claude"),
        unique,
        steps,
        exit_wait: 10,
    }
}

fn agent_scenarios() -> Vec<Scenario> {
    let mut startup = vec![
        Wait("Yes, I trust this folder"),
        Idle(1500),
        Mark("trust-dialog"),
        Key("down"),
        WaitRe(r"❯ Yes, I trust this folder"),
        Idle(700),
        Mark("trust-yes-selected"),
        Key("enter"),
        WaitGone("Yes, I trust this folder"),
    ];
    startup.extend(claude_ready());
    startup.extend([
        Type("?"),
        Idle(1500),
        Mark("shortcuts"),
        Key("backspace"),
        Idle(1000),
        Mark("shortcuts-closed"),
        Resize(80, 24),
        Idle(1500),
        Mark("resized-80x24"),
        Resize(120, 36),
        Idle(1500),
        Mark("resized-120x36"),
    ]);
    startup.extend(claude_exit());

    let mut prompt = claude_ready();
    prompt.extend([
        Type("Count from 1 to 10, one number per line, nothing else."),
        Wait("❯ Count from 1 to 10"),
        Idle(700),
        Mark("prompt-typed"),
        Key("enter"),
        Wait("esc to interrupt"),
        Mark("working"),
        WaitGoneLong("esc to interrupt"),
        Idle(2000),
        Mark("response"),
        Type("Write a 40 line poem about terminal emulators."),
        Wait("❯ Write a 40 line poem"),
        Idle(500),
        Key("enter"),
        Wait("esc to interrupt"),
        Idle(1500),
        Mark("working-2"),
        Key("esc"),
        WaitGone("esc to interrupt"),
        Idle(2000),
        Mark("interrupted"),
    ]);
    prompt.extend(claude_exit());

    let mut permission = claude_ready();
    permission.extend([
        Type("Use the Bash tool to run exactly this command: echo hello > hello.txt"),
        Wait("❯ Use the Bash tool"),
        Idle(500),
        Key("enter"),
        Wait("esc to interrupt"),
        Mark("working"),
        WaitLong("Do you want to proceed?"),
        Idle(1500),
        Mark("permission-prompt"),
        Key("down"),
        WaitRe(r"❯ 2\. Yes, and"),
        Idle(1000),
        Mark("permission-option-2"),
        Key("up"),
        WaitRe(r"❯ 1\. Yes"),
        Idle(500),
        Key("enter"),
        WaitGone("Do you want to proceed?"),
        Mark("approved"),
        WaitGoneLong("esc to interrupt"),
        Idle(2000),
        Mark("done"),
        Type("Use the Bash tool to run exactly this command: rm hello.txt"),
        Wait("❯ Use the Bash tool to run exactly this command: rm"),
        Idle(500),
        Key("enter"),
        WaitLong("Do you want to proceed?"),
        Idle(1500),
        Mark("permission-prompt-2"),
        Key("esc"),
        WaitGone("Do you want to proceed?"),
        Idle(2500),
        Mark("rejected"),
    ]);
    permission.extend(claude_exit());

    let mut slash = claude_ready();
    slash.extend([
        Type("/"),
        Wait("/add-dir"),
        Idle(1200),
        Mark("slash-menu"),
        Key("down"),
        Idle(300),
        Key("down"),
        Idle(1000),
        Mark("slash-menu-moved"),
        Type("mo"),
        Wait("❯ /mo"),
        Idle(1200),
        Mark("slash-menu-filtered"),
        Key("esc"),
        Idle(1200),
        Mark("slash-menu-closed"),
        Key("backspace"),
        Key("backspace"),
        Key("backspace"),
        Idle(1000),
        Mark("input-cleared"),
    ]);
    slash.extend(claude_exit());

    let mut model = claude_ready();
    model.extend([
        Type("/model"),
        Wait("❯ /model"),
        Idle(1000),
        Key("enter"),
        WaitRe(r"(?i)select model"),
        Idle(1500),
        Mark("model-picker"),
        Key("down"),
        Idle(1000),
        Mark("model-picker-moved"),
        Key("esc"),
        WaitGone("Select model"),
        Idle(1500),
        Mark("model-picker-closed"),
    ]);
    model.extend(claude_exit());

    vec![
        claude(
            "claude-startup",
            "Claude Code in a new folder: trust dialog, idle prompt, shortcuts help, two resizes, quit",
            true,
            startup,
        ),
        claude(
            "claude-prompt",
            "Claude Code: type a prompt, working spinner, streamed answer; a second prompt interrupted with Esc",
            false,
            prompt,
        ),
        claude(
            "claude-permission",
            "Claude Code: a Bash permission dialog, moving the selection, approving; a second one rejected with Esc",
            false,
            permission,
        ),
        claude(
            "claude-slash",
            "Claude Code: the slash command menu, moving in it, filtering, closing",
            false,
            slash,
        ),
        claude(
            "claude-model",
            "Claude Code: the /model picker, moving in it, closing without a change",
            false,
            model,
        ),
        Scenario {
            name: "codex",
            about: "Codex without sending a prompt: logo animation, idle prompt, shortcuts, slash menu, model picker, quit",
            size: Size::new(30, 100),
            cmd: &["codex"],
            files: &[],
            fresh: true,
            group: Some("codex"),
            unique: false,
            steps: vec![
                // Codex sometimes opens with an update dialog; Enter there would run npm.
                IfSeen("Update available", 4000, &["esc"]),
                Wait("Ask Codex to do anything"),
                Mark("idle"),
                Idle(4000),
                Mark("idle-settled"),
                Type("?"),
                Wait("Keyboard shortcuts"),
                Idle(1500),
                Mark("shortcuts"),
                Key("esc"),
                WaitGone("Keyboard shortcuts"),
                Idle(1000),
                Type("/"),
                Wait("/model"),
                Idle(1500),
                Mark("slash-menu"),
                Key("down"),
                Idle(800),
                Mark("slash-menu-moved"),
                Key("esc"),
                Idle(800),
                Key("backspace"),
                Idle(1000),
                Mark("slash-menu-closed"),
                Type("/model"),
                Wait("› /model"),
                Idle(800),
                Key("enter"),
                Idle(2500),
                Mark("model-picker"),
                Key("esc"),
                Idle(1500),
                Mark("model-picker-closed"),
                Key("ctrl+c"),
                Idle(1500),
                Mark("exit"),
            ],
            exit_wait: 5,
        },
    ]
}

/// Claude Code in plan mode.
const CLAUDE_PLAN: &[&str] = &[
    "claude",
    "--model",
    "haiku",
    "--permission-mode",
    "plan",
    "--setting-sources",
    "project,local",
];

/// Codex with its cheapest model, asking before commands that leave the read-only sandbox, and
/// without the shared app-server daemon.
const CODEX: &[&str] = &[
    "codex",
    "--no-daemon",
    "-m",
    "gpt-6-luna",
    "-a",
    "on-request",
    "-s",
    "read-only",
];

/// Codex in a new git repository, where it asks whether to trust the folder.
const CODEX_GIT: &[&str] = &[
    "bash",
    "-c",
    "git init -q && exec codex --no-daemon -m gpt-6-luna -a on-request -s read-only",
];

/// Codex's first-run dialogs in a new folder: the update notice (skipped) and the folder trust
/// dialog (accepted), then the idle prompt.
fn codex_ready() -> Vec<Step> {
    vec![
        IfSeen("Update available", 5000, &["esc"]),
        IfSeen("Trust this folder", 6000, &["enter"]),
        Wait("Ask Codex to do anything"),
        Idle(2000),
        Mark("idle"),
    ]
}

fn codex_exit() -> Vec<Step> {
    vec![
        Key("ctrl+c"),
        WaitGone("Ask Codex to do anything"),
        Idle(1000),
        Mark("exit"),
    ]
}

fn codex(name: &'static str, about: &'static str, size: Size, steps: Vec<Step>) -> Scenario {
    Scenario {
        name,
        about,
        size,
        cmd: CODEX,
        files: &[],
        fresh: true,
        group: None,
        unique: true,
        steps,
        exit_wait: 8,
    }
}

fn claude2(
    name: &'static str,
    about: &'static str,
    size: Size,
    cmd: &'static [&'static str],
    files: &'static [(&'static str, &'static str)],
    steps: Vec<Step>,
) -> Scenario {
    Scenario {
        name,
        about,
        size,
        cmd,
        files,
        fresh: false,
        group: Some("claude2"),
        unique: false,
        steps,
        exit_wait: 10,
    }
}

fn with(mut a: Vec<Step>, b: impl IntoIterator<Item = Step>) -> Vec<Step> {
    a.extend(b);
    a
}

/// Steps that submit a prompt to Claude Code and wait until it starts working.
fn claude_submit(text: &'static str, shown: &'static str) -> Vec<Step> {
    vec![
        Type(text),
        Wait(shown),
        Idle(500),
        Key("enter"),
        Wait("esc to interrupt"),
    ]
}

fn more_agent_scenarios() -> Vec<Scenario> {
    let big = Size::new(30, 100);
    let codex_prompt = with(
        codex_ready(),
        [
            Type("Count from 1 to 10, one number per line, nothing else."),
            Wait("› Count from 1 to 10"),
            Idle(700),
            Mark("prompt-typed"),
            Key("enter"),
            Wait("esc to interrupt"),
            Mark("working"),
            WaitGoneLong("esc to interrupt"),
            Idle(2000),
            Mark("response"),
            Type("Write a 40 line poem about terminal emulators."),
            Wait("› Write a 40 line poem"),
            Idle(500),
            Key("enter"),
            Wait("esc to interrupt"),
            Idle(3000),
            Mark("working-2"),
            Key("esc"),
            WaitGone("esc to interrupt"),
            Idle(2000),
            Mark("interrupted"),
        ],
    );
    let codex_approval = with(
        codex_ready(),
        [
            Type("Use a shell command to create a file hello.txt containing the word hello."),
            Wait("› Use a shell command"),
            Idle(500),
            Key("enter"),
            Wait("esc to interrupt"),
            Mark("working"),
            WaitLong("Would you like to run"),
            Idle(1500),
            Mark("approval"),
            Key("down"),
            WaitRe(r"› 2\. Yes"),
            Idle(800),
            Mark("approval-option-2"),
            Key("up"),
            WaitRe(r"› 1\. Yes"),
            Idle(500),
            Key("enter"),
            WaitGone("Would you like to run"),
            Mark("approved"),
            WaitGoneLong("esc to interrupt"),
            Idle(2000),
            Mark("done"),
            Type("Use a shell command to delete hello.txt."),
            Wait("› Use a shell command to delete"),
            Idle(500),
            Key("enter"),
            WaitLong("Would you like to run"),
            Idle(1500),
            Mark("approval-2"),
            Key("esc"),
            WaitGone("Would you like to run"),
            WaitGoneLong("esc to interrupt"),
            Idle(2500),
            Mark("denied"),
        ],
    );
    let codex_menus = with(
        codex_ready(),
        [
            Type("?"),
            Wait("Keyboard shortcuts"),
            Idle(1500),
            Mark("shortcuts"),
            Key("esc"),
            WaitGone("Keyboard shortcuts"),
            Idle(1000),
            Type("/"),
            Wait("/model"),
            Idle(1500),
            Mark("slash-menu"),
            Key("down"),
            Idle(800),
            Mark("slash-menu-moved"),
            Type("mo"),
            Wait("› /mo"),
            Idle(1000),
            Mark("slash-menu-filtered"),
            Key("esc"),
            Idle(800),
            Key("backspace"),
            Key("backspace"),
            Key("backspace"),
            Idle(1000),
            Mark("slash-menu-closed"),
            Type("/model"),
            Wait("› /model"),
            Idle(800),
            Key("enter"),
            Wait("Select Model and Effort"),
            Idle(1500),
            Mark("model-picker"),
            Key("down"),
            Idle(1000),
            Mark("model-picker-moved"),
            Key("esc"),
            WaitGone("Select Model"),
            Idle(1500),
            Mark("model-picker-closed"),
        ],
    );
    let codex_holdout = with(
        codex_ready(),
        [
            Type("What is 2+2? Answer with one word."),
            Wait("› What is 2+2"),
            Idle(500),
            Key("enter"),
            Wait("esc to interrupt"),
            Mark("working"),
            WaitGoneLong("esc to interrupt"),
            Idle(2000),
            Mark("answered"),
            Type(
                "Use a shell command to create a file x.txt containing the letter x. If the sandbox blocks it, request approval to run it outside the sandbox.",
            ),
            Wait("› Use a shell command"),
            Idle(500),
            Key("enter"),
            WaitLong("Would you like to run"),
            Idle(1500),
            Mark("approval"),
            Key("enter"),
            WaitGone("Would you like to run"),
            WaitGoneLong("esc to interrupt"),
            Idle(2000),
            Mark("done"),
            Type("/model"),
            Wait("› /model"),
            Idle(800),
            Key("enter"),
            Wait("Select Model and Effort"),
            Idle(1500),
            Mark("model-picker"),
            Key("esc"),
            WaitGone("Select Model"),
            Idle(1500),
            Mark("model-picker-closed"),
        ],
    );
    let question = with(
        claude_ready(),
        with(
            claude_submit(
                "Use the AskUserQuestion tool to ask me which colour I prefer, with the options red, green and blue. Then reply with just my choice.",
                "❯ Use the AskUserQuestion tool",
            ),
            [
                Mark("working"),
                WaitLong("Enter to select"),
                Idle(1500),
                Mark("question"),
                Key("down"),
                WaitRe(r"❯ 2\. Green"),
                Idle(1000),
                Mark("question-option-2"),
                Key("enter"),
                WaitGone("Enter to select"),
                Mark("answered"),
                WaitGoneLong("esc to interrupt"),
                Idle(2000),
                Mark("done"),
            ],
        ),
    );
    let plan = with(
        vec![
            IfSeen("Yes, I trust this folder", 4000, &["down", "enter"]),
            Wait("plan mode on"),
            Idle(2500),
            Mark("idle"),
        ],
        with(
            claude_submit(
                "Make a plan to create a file named plan.txt containing the word hi. Keep the plan to two steps, do not explore.",
                "❯ Make a plan",
            ),
            [
                Mark("working"),
                WaitLong("Would you like to proceed?"),
                Idle(1500),
                Mark("plan-approval"),
                Key("esc"),
                WaitGone("Would you like to proceed?"),
                WaitGoneLong("esc to interrupt"),
                Idle(2000),
                Mark("plan-rejected"),
                Type("/exit"),
                Wait("❯ /exit"),
                Idle(800),
                Mark("exit-typed"),
                Key("enter"),
                WaitGone("plan mode on"),
            ],
        ),
    );
    let long = with(
        claude_ready(),
        with(
            claude_submit(
                "Print the numbers from 1 to 80, one per line, nothing else.",
                "❯ Print the numbers",
            ),
            [
                Mark("working"),
                WaitGoneLong("esc to interrupt"),
                Idle(2000),
                Mark("done"),
            ],
        ),
    );
    let edit = with(
        claude_ready(),
        with(
            claude_submit(
                "Use the Edit tool to replace the word apple with pear in notes.txt.",
                "❯ Use the Edit tool",
            ),
            [
                Mark("working"),
                WaitLong("Do you want to make this edit"),
                Idle(1500),
                Mark("edit-approval"),
                Key("enter"),
                WaitGone("Do you want to make this edit"),
                Mark("approved"),
                WaitGoneLong("esc to interrupt"),
                Idle(2000),
                Mark("done"),
            ],
        ),
    );
    let resize = with(
        claude_ready(),
        with(
            claude_submit("Write a 30 line poem about terminals.", "❯ Write a 30 line poem"),
            [
                Idle(1500),
                Mark("working"),
                Resize(80, 24),
                Idle(1500),
                Mark("resized-80x24"),
                Resize(120, 36),
                Idle(1500),
                Mark("resized-120x36"),
                WaitGoneLong("esc to interrupt"),
                Idle(2000),
                Mark("done"),
            ],
        ),
    );
    let narrow = with(
        claude_ready(),
        with(
            claude_submit(
                "Use the Bash tool to run exactly this command: echo narrow > n.txt",
                "❯ Use the Bash tool",
            ),
            [
                Mark("working"),
                WaitLong("Do you want to proceed?"),
                Idle(1500),
                Mark("permission-prompt"),
                Key("enter"),
                WaitGone("Do you want to proceed?"),
                Mark("approved"),
                WaitGoneLong("esc to interrupt"),
                Idle(2000),
                Mark("done"),
            ],
        ),
    );
    let holdout1 = with(
        claude_ready(),
        with(
            claude_submit(
                "Use the Bash tool to run exactly this command: echo held > held.txt",
                "❯ Use the Bash tool",
            ),
            [
                Mark("working"),
                WaitLong("Do you want to proceed?"),
                Idle(1500),
                Mark("permission-prompt"),
                Key("esc"),
                WaitGone("Do you want to proceed?"),
                Idle(2500),
                Mark("rejected"),
                Type("Write a 50 line story about a lighthouse."),
                Wait("❯ Write a 50 line story"),
                Idle(500),
                Key("enter"),
                Wait("esc to interrupt"),
                Idle(2500),
                Mark("working-2"),
                Key("esc"),
                WaitGone("esc to interrupt"),
                Idle(2000),
                Mark("interrupted"),
            ],
        ),
    );
    let holdout2 = with(
        claude_ready(),
        [
            Type("/model"),
            Wait("❯ /model"),
            Idle(1000),
            Key("enter"),
            WaitRe(r"(?i)select model"),
            Idle(1500),
            Mark("model-picker"),
            Key("esc"),
            WaitGone("Select model"),
            Idle(1500),
            Mark("model-picker-closed"),
            Type(
                "Use the AskUserQuestion tool to ask me whether I want tea or coffee. Then reply with just my choice.",
            ),
            Wait("❯ Use the AskUserQuestion tool"),
            Idle(500),
            Key("enter"),
            Wait("esc to interrupt"),
            Mark("working"),
            WaitLong("Enter to select"),
            Idle(1500),
            Mark("question"),
            Key("enter"),
            WaitGone("Enter to select"),
            WaitGoneLong("esc to interrupt"),
            Idle(2000),
            Mark("done"),
        ],
    );
    let codex_trust = vec![
        IfSeen("Update available", 5000, &["esc"]),
        Wait("Trust this folder"),
        Idle(1500),
        Mark("trust-dialog"),
        Key("enter"),
        WaitGone("Trust this folder"),
        Wait("Ask Codex to do anything"),
        Idle(2000),
        Mark("idle"),
        Type("Say hi in one word."),
        Wait("› Say hi"),
        Idle(500),
        Key("enter"),
        Wait("esc to interrupt"),
        Mark("working"),
        WaitGoneLong("esc to interrupt"),
        Idle(2500),
        Mark("done"),
    ];
    let mut trust = codex(
        "codex-trust",
        "Codex in a new git repository: the folder trust dialog accepted, a one-word answer; quit",
        big,
        with(codex_trust, codex_exit()),
    );
    trust.cmd = CODEX_GIT;
    vec![
        trust,
        codex(
            "codex-prompt",
            "Codex: first-run dialogs, a prompt, the working line, a streamed answer; a second prompt interrupted with Esc; quit",
            big,
            with(codex_prompt, codex_exit()),
        ),
        codex(
            "codex-approval",
            "Codex: a command approval moved, approved, done; a second one denied with Esc; quit",
            big,
            with(codex_approval, codex_exit()),
        ),
        codex(
            "codex-menus",
            "Codex: shortcuts, the slash menu moved and filtered, the model picker moved and closed; quit",
            big,
            with(codex_menus, codex_exit()),
        ),
        codex(
            "codex-holdout",
            "Held out: Codex at 90x28: a short answer, an approval, the model picker; quit",
            Size::new(28, 90),
            with(codex_holdout, codex_exit()),
        ),
        claude2(
            "claude-question",
            "Claude Code: AskUserQuestion with three colours, moved and answered",
            big,
            CLAUDE,
            &[],
            with(question, claude_exit()),
        ),
        claude2(
            "claude-plan",
            "Claude Code in plan mode: a plan, the plan approval dialog, rejected with Esc",
            big,
            CLAUDE_PLAN,
            &[],
            plan,
        ),
        claude2(
            "claude-long",
            "Claude Code: an 80-line answer that scrolls",
            big,
            CLAUDE,
            &[],
            with(long, claude_exit()),
        ),
        claude2(
            "claude-edit",
            "Claude Code: an Edit tool approval for notes.txt, approved",
            big,
            CLAUDE,
            &[("notes.txt", "apple\nbanana\n")],
            with(edit, claude_exit()),
        ),
        claude2(
            "claude-resize",
            "Claude Code: resized to 80x24 and 120x36 while it works",
            big,
            CLAUDE,
            &[],
            with(resize, claude_exit()),
        ),
        claude2(
            "claude-narrow",
            "Claude Code at 60x24: a Bash permission dialog, approved",
            Size::new(24, 60),
            CLAUDE,
            &[],
            with(narrow, claude_exit()),
        ),
        claude2(
            "claude-holdout-1",
            "Held out: Claude Code at 90x26: a permission dialog rejected, a long answer interrupted",
            Size::new(26, 90),
            CLAUDE,
            &[],
            with(holdout1, claude_exit()),
        ),
        claude2(
            "claude-holdout-2",
            "Held out: Claude Code: the model picker, then a question answered with Enter",
            big,
            CLAUDE,
            &[],
            with(holdout2, claude_exit()),
        ),
    ]
}
