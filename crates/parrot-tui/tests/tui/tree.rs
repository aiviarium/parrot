//! The labelled tree on recordings and on a scripted program: state timelines against the
//! truth files, stable ids, generic surfaces, and actions with require and expect.

use parrot_tui::SessionExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use parrot_term::io::{SinkEvent, channel};
use parrot_term::{Cast, Replay, ReplayConfig, Session, SessionConfig, SettleConfig, Size};
use parrot_tui::measure::{DEFAULT_TOLERANCE, Summary, measure_file};
use parrot_tui::{Profile, Query, Role, SurfaceKind, Tracker, TuiErrorKind, TuiEvent};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn truth_files(sub: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir().join(sub))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".truth.toml"))
        .collect();
    v.sort();
    v
}

/// Frozen copies of the agent package's profiles, so these tests do not move when those do.
fn profile(name: &str) -> Profile {
    Profile::load(dir().join("fixtures/profiles").join(name)).unwrap()
}

/// Every recording with a truth file replays to the labelled state timeline: every marker
/// shows its state, no transition is missed, and disagreements are only detection lag.
#[test]
fn state_timelines_match_the_truth() {
    for (sub, holdout) in [("fixtures/casts", false), ("fixtures/casts/holdout", true)] {
        let files = truth_files(sub);
        assert!(!files.is_empty(), "no truth files in {sub}");
        let mut by_profile: HashMap<String, Summary> = HashMap::new();
        for f in files {
            let r = measure_file(&f, SettleConfig::for_automation(), DEFAULT_TOLERANCE).unwrap();
            assert_eq!(r.holdout, holdout, "{}", f.display());
            let wrong: Vec<_> = r
                .intervals
                .iter()
                .filter(|i| !i.timing && i.end - i.start > 0.1)
                .collect();
            assert!(wrong.is_empty(), "{}:\n{}", f.display(), r.to_text());
            assert!(
                r.transitions.iter().all(|t| t.latency.is_some_and(|l| l < 0.5)),
                "{}:\n{}",
                f.display(),
                r.to_text()
            );
            if !holdout {
                assert!(
                    r.markers.iter().all(|m| m.expected == m.detected),
                    "{}:\n{}",
                    f.display(),
                    r.to_text()
                );
            }
            assert_eq!(r.unrecognised, 0, "{}", f.display());
            by_profile.entry(r.profile.clone()).or_default().add(&r);
        }
        for (p, s) in &by_profile {
            let agree: f64 = s.states.values().map(|v| v.both).sum::<f64>() / s.scored;
            assert!(agree >= 0.95, "{p} {sub}: {}", s.to_text(p));
        }
    }
}

fn frames(cast: &str) -> Vec<parrot_term::Frame> {
    let cast = Cast::load(dir().join("fixtures/casts").join(cast)).unwrap();
    Replay::with_config(
        &cast,
        ReplayConfig {
            settle: SettleConfig::for_automation(),
            ..Default::default()
        },
    )
    .frames()
}

#[test]
fn the_prompt_keeps_its_id_across_frames() {
    let mut t = Tracker::new(Some(profile("claude-code.kdl")), Default::default());
    let mut ids = Vec::new();
    for f in frames("claude-prompt.cast") {
        t.update(f.time, &f.screen);
        if let Some(p) = t.tree().and_then(|tree| tree.find_tag("prompt")) {
            ids.push(p.id.clone());
        }
    }
    assert!(ids.len() > 10);
    let first = &ids[0];
    let same = ids.iter().filter(|i| *i == first).count();
    assert!(same * 10 >= ids.len() * 9, "prompt ids: {ids:?}");
}

#[test]
fn dialogs_are_generic_surfaces_without_a_profile() {
    for (cast, at, options, selected) in [
        ("claude-permission.cast", 10.4, 4, Some(1)),
        ("claude-startup.cast", 1.8, 2, Some(1)),
        ("codex-approval.cast", 17.1, 3, Some(1)),
        ("codex-menus.cast", 19.0, 8, Some(5)),
    ] {
        let c = Cast::load(dir().join("fixtures/casts").join(cast)).unwrap();
        let mut t = Tracker::new(None, Default::default());
        t.update(at, &Replay::new(&c).screen_at(at));
        let s = t.snapshot().surfaces;
        let choice: Vec<_> = s.iter().filter(|s| s.kind == SurfaceKind::Choice).collect();
        assert_eq!(choice.len(), 1, "{cast}: {s:?}");
        assert_eq!(choice[0].options.len(), options, "{cast}: {:?}", choice[0]);
        assert_eq!(choice[0].selected, selected, "{cast}");
    }
}

#[test]
fn trees_diff_into_events() {
    let mut t = Tracker::new(Some(profile("codex.kdl")), Default::default());
    let mut states = Vec::new();
    let mut added = 0;
    for f in frames("codex-approval.cast") {
        for e in t.update(f.time, &f.screen) {
            match e {
                TuiEvent::State { to, .. } => states.push(to),
                TuiEvent::Tree { diff, .. } => added += diff.added.len(),
                _ => {}
            }
        }
    }
    assert!(added > 0);
    let states: Vec<_> = states.into_iter().flatten().collect();
    assert!(states.windows(2).any(|w| w == ["working", "awaiting-approval"]));
    assert!(states.windows(2).any(|w| w == ["awaiting-approval", "working"]));
}

/// A program drawn like Claude Code: a prompt between two rules, a working spinner, and a
/// permission dialog with two options.
async fn fake_cli(
    out: tokio::sync::mpsc::Sender<Vec<u8>>,
    mut inp: tokio::sync::mpsc::UnboundedReceiver<SinkEvent>,
) {
    #[derive(PartialEq)]
    enum Mode {
        Idle,
        Working(u8),
        Approval(usize),
    }
    let rule = "─".repeat(40);
    let mut mode = Mode::Idle;
    let mut input = String::new();
    let mut log = String::from("● ready");
    loop {
        let mut s = String::from("\x1b[H\x1b[2J");
        let mut cursor = None;
        match &mode {
            Mode::Idle | Mode::Working(_) => {
                s.push_str(&format!("\x1b[1;1H{log}"));
                if matches!(mode, Mode::Working(_)) {
                    s.push_str("\x1b[7;1H✢ Thinking…");
                }
                s.push_str(&format!("\x1b[9;1H{rule}\x1b[10;1H❯ {input}\x1b[11;1H{rule}"));
                let hint = if mode == Mode::Idle {
                    "? for shortcuts"
                } else {
                    "esc to interrupt"
                };
                s.push_str(&format!("\x1b[12;3H⏸ manual mode on · {hint}"));
                cursor = Some((10, 3 + input.chars().count()));
            }
            Mode::Approval(sel) => {
                s.push_str(&format!("\x1b[1;1H{log}\x1b[4;1H{rule}"));
                s.push_str("\x1b[5;2HBash command\x1b[6;2HDo you want to proceed?");
                for (i, o) in ["Yes", "No"].iter().enumerate() {
                    let m = if i == *sel { "❯" } else { " " };
                    s.push_str(&format!("\x1b[{};2H{m} {}. {o}", 7 + i, i + 1));
                }
                s.push_str("\x1b[10;2HEsc to cancel");
            }
        }
        match cursor {
            Some((r, c)) => s.push_str(&format!("\x1b[{r};{c}H\x1b[?25h")),
            None => s.push_str("\x1b[?25l"),
        }
        if out.send(s.into_bytes()).await.is_err() {
            return;
        }
        let tick = async {
            match mode {
                Mode::Working(_) => tokio::time::sleep(Duration::from_millis(300)).await,
                _ => std::future::pending().await,
            }
        };
        tokio::select! {
            ev = inp.recv() => {
                let Some(SinkEvent::Data(d)) = ev else { if ev.is_none() { return } else { continue } };
                match (&mut mode, d.as_slice()) {
                    (Mode::Idle, b"\r") => { log = format!("❯ {input}"); input.clear(); mode = Mode::Working(0) }
                    (Mode::Idle, b"\x1b") => {}
                    (Mode::Idle, t) => input.push_str(&String::from_utf8_lossy(t)),
                    (Mode::Working(_), b"\x1b") => { log = "⎿ Interrupted".into(); mode = Mode::Idle }
                    (Mode::Approval(s), b"\x1b[B") => *s = (*s + 1).min(1),
                    (Mode::Approval(s), b"\x1b[A") => *s = s.saturating_sub(1),
                    (Mode::Approval(0), b"\r") => { log = "● ran it".into(); mode = Mode::Working(2) }
                    (Mode::Approval(_), b"\r" | b"\x1b") => { log = "● rejected".into(); mode = Mode::Idle }
                    _ => {}
                }
            }
            _ = tick => {
                mode = match mode {
                    Mode::Working(0) => Mode::Approval(0),
                    Mode::Working(n) if n >= 3 => { log = "● done".into(); Mode::Idle }
                    Mode::Working(n) => Mode::Working(n + 1),
                    m => m,
                };
            }
        }
    }
}

#[tokio::test]
async fn actions_drive_a_program_and_fail_loudly() {
    let (source, sink, out, inp) = channel();
    let session = Session::new(source, sink, SessionConfig::for_automation(Size::new(12, 40)));
    tokio::spawn(fake_cli(out, inp));
    let tui = session.automate(Some(profile("claude-code.kdl")));
    let t = Duration::from_secs(5);
    tui.wait_for_state("idle", t).await.unwrap();

    // Approving while idle is refused before any key is sent, with the tree in the error.
    let e = tui.act("approve", &HashMap::new()).await.unwrap_err();
    assert_eq!(e.kind, TuiErrorKind::Require);
    let text = e.to_string();
    assert!(
        text.contains("require failed") && text.contains("input"),
        "{text}"
    );
    assert!(tui.act("submit", &HashMap::new()).await.is_err(), "missing $text");

    let vars = HashMap::from([("text".to_string(), "run it".to_string())]);
    tui.act("submit", &vars).await.unwrap();
    tui.wait_for_state("awaiting-approval", t).await.unwrap();
    let choices = tui.find(&Query::tag("choices"));
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].items().len(), 2);
    tui.act("approve", &HashMap::new()).await.unwrap();
    tui.wait_for_state("idle", t).await.unwrap();
    assert!(
        tui.find(&Query::role(Role::Text))
            .iter()
            .any(|n| n.text.contains("done"))
    );

    // Choosing "No" moves the selection down one step and confirms.
    tui.act("submit", &vars).await.unwrap();
    tui.wait_for_state("awaiting-approval", t).await.unwrap();
    let opt = HashMap::from([("option".to_string(), "2".to_string())]);
    tui.act("choose", &opt).await.unwrap();
    tui.wait_for_state("idle", t).await.unwrap();
    let q = Query {
        text: Some("rejected".into()),
        role: Some(Role::Text),
        ..Default::default()
    };
    assert_eq!(tui.find(&q).len(), 1);

    // Interrupting while working.
    tui.act("submit", &vars).await.unwrap();
    tui.wait_for_state("awaiting-approval", t).await.unwrap();
    tui.act("approve", &HashMap::new()).await.unwrap();
    tui.act("interrupt", &HashMap::new()).await.unwrap();
    assert_eq!(tui.state().as_deref(), Some("idle"));

    // An option that does not exist fails the step and names the options.
    tui.act("submit", &vars).await.unwrap();
    tui.wait_for_state("awaiting-approval", t).await.unwrap();
    let bad = HashMap::from([("option".to_string(), "Maybe".to_string())]);
    let e = tui.act("choose", &bad).await.unwrap_err();
    assert_eq!(e.kind, TuiErrorKind::Step);
    assert!(e.message.contains("Yes | No"), "{}", e.message);
    tui.act("deny", &HashMap::new()).await.unwrap();

    // Without a profile the dialog is still a surface that can be answered.
    tui.reload(None);
    assert_eq!(
        tui.state(),
        None,
        "a reload applies to the current screen at once"
    );
    tui.act("submit", &vars).await.unwrap_err();
    session.type_text("x").await.unwrap();
    session.press("enter").await.unwrap();
    let cond = parrot_tui::profile::Cond {
        any_text_matches: Some(parrot_tui::profile::Re(regex::Regex::new("Do you want").unwrap())),
        ..Default::default()
    };
    tui.wait_for(&cond, t).await.unwrap();
    let s = tui.surfaces();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].options[1].label, "No");
    tui.choose(&s[0].id, 1).await.unwrap();
    let gone = parrot_tui::profile::Cond {
        any_text_matches: Some(parrot_tui::profile::Re(regex::Regex::new("rejected").unwrap())),
        ..Default::default()
    };
    tui.wait_for(&gone, t).await.unwrap();
}
