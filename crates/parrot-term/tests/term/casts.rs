//! Tests against the recordings in `fixtures/casts/`.

use std::path::PathBuf;

use parrot_term::{Cast, CellKind, Emulator, EmulatorConfig, Replay, Screen, SettleReason, serialize};

fn cast(name: &str) -> Cast {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/casts")
        .join(format!("{name}.cast"));
    Cast::load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn all_casts() -> Vec<(String, Cast)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/casts");
    let mut v: Vec<(String, Cast)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "cast"))
        .map(|e| {
            let name = e.path().file_stem().unwrap().to_string_lossy().to_string();
            (name.clone(), cast(&name))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(v.len() >= 13, "expected the recorded casts, found {}", v.len());
    v
}

/// Screen at a marker: just after it.
fn at(cast: &Cast, marker: &str) -> Screen {
    let t = cast
        .markers()
        .into_iter()
        .find(|(_, m)| m == marker)
        .unwrap_or_else(|| panic!("no marker {marker}"))
        .0;
    Replay::new(cast).screen_at(t)
}

fn check_serialisation(name: &str, s: &Screen) {
    let mut e = Emulator::new(EmulatorConfig::with_size(s.size()));
    e.feed(&serialize(s));
    let r = e.screen();
    assert_eq!(r.rows(), s.rows(), "{name}: rows");
    assert_eq!(r.cursor(), s.cursor(), "{name}: cursor");
    assert_eq!(r.modes(), s.modes(), "{name}: modes");
    assert_eq!(r.title(), s.title(), "{name}: title");
    assert_eq!(r.saved_primary(), s.saved_primary(), "{name}: saved primary");
    let ha: Vec<_> = s.history().iter().collect();
    let hb: Vec<_> = r.history().iter().collect();
    assert_eq!(ha, hb, "{name}: history");
}

#[test]
fn every_cast_parses_writes_back_and_has_markers() {
    for (name, c) in all_casts() {
        assert_eq!(Cast::parse(&c.to_text()).unwrap(), c, "{name}");
        assert!(!c.markers().is_empty(), "{name} has no markers");
    }
}

#[test]
fn replay_is_deterministic() {
    for (name, c) in all_casts() {
        let a = Replay::new(&c).frames();
        let b = Replay::new(&c).frames();
        assert!(!a.is_empty(), "{name}");
        assert_eq!(a.len(), b.len(), "{name}");
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.time, y.time, "{name}");
            assert_eq!(x.reason, y.reason, "{name}");
            assert!(x.screen.same_content(&y.screen), "{name} at {}", x.time);
        }
        // Frames never go back in time, and every scrolled line is reported exactly once.
        assert!(a.windows(2).all(|w| w[0].time <= w[1].time), "{name}");
        let total: u64 = a.iter().map(|f| f.new_lines.len() as u64 + f.lost_lines).sum();
        assert_eq!(total, Replay::new(&c).final_screen().first_line(), "{name}");
    }
}

#[test]
fn serialising_every_marked_screen_reproduces_it() {
    for (name, c) in all_casts() {
        for (t, m) in c.markers() {
            let s = Replay::new(&c).screen_at(t);
            check_serialisation(&format!("{name}@{m}"), &s);
        }
        check_serialisation(&format!("{name}@end"), &Replay::new(&c).final_screen());
    }
}

#[test]
fn shell_keeps_combining_marks_wide_chars_and_emoji() {
    // Regression: novalia's screen model dropped combining marks.
    let c = cast("shell-unicode");
    let s = at(&c, "styles-shown");
    let row = |needle: &str| {
        s.rows()
            .iter()
            .find(|r| r.text().starts_with(needle))
            .unwrap_or_else(|| panic!("no row {needle}"))
            .clone()
    };
    let r = row("e\u{301}");
    assert_eq!(r.cell(0).text(), "e\u{301}");
    assert_eq!(r.cell(2).text(), "a\u{308}");
    assert_eq!(r.cell(6).text(), "o\u{302}\u{323}");
    assert_eq!(r.cell(8).text(), "x\u{20d7}");
    let r = row("界 世界");
    assert_eq!(r.cell(0).kind(), CellKind::Wide);
    assert_eq!(r.cell(1).kind(), CellKind::WideTail);
    assert!(r.text().contains("ｆｕｌｌ"));
    let r = row("🎉");
    assert!(r.text().contains("👍🏽"));
    assert!(r.text().contains("❤\u{fe0f}"));
    assert!(r.text().contains("👨\u{200d}👩\u{200d}👧"));
    assert!(r.text().contains("🇩🇪"));
    // 59 x's, then 界 does not fit in the last column: a spacer, and it wraps.
    let i = s
        .rows()
        .iter()
        .position(|r| r.text().starts_with("xxxx"))
        .unwrap();
    let r = &s.rows()[i];
    assert!(r.is_wrapped());
    assert_eq!(r.cell(59).kind(), CellKind::WrapSpacer);
    assert!(s.rows()[i + 1].text().starts_with("界 wraps"));
    let styled = row("bold dim");
    assert!(styled.cell(0).style().bold());
    assert!(styled.cell(5).style().dim());
    assert!(styled.cell(9).style().italic());
}

#[test]
fn burst_of_5000_lines_loses_nothing() {
    // Regression: novalia lost history when more than 1000 lines arrived between two frames.
    let c = cast("burst-5000");
    let frames = Replay::new(&c).frames();
    let lines: Vec<String> = frames
        .iter()
        .flat_map(|f| f.new_lines.iter().map(|r| r.text()))
        .collect();
    let numbered: Vec<&String> = lines.iter().filter(|l| l.starts_with("line ")).collect();
    // Every line that scrolled off is reported, in order, without gaps.
    for (i, l) in numbered.iter().enumerate() {
        assert_eq!(**l, format!("line {} of 5000", i + 1));
    }
    let s = Replay::new(&c).final_screen();
    let visible: Vec<String> = s.lines().into_iter().filter(|l| l.starts_with("line ")).collect();
    assert_eq!(numbered.len() + visible.len(), 5000);
    assert_eq!(visible.last().unwrap(), "line 5000 of 5000");
    // And the scrollback of the final screen has them too.
    assert!(s.history().iter().any(|r| r.text() == "line 1 of 5000"));
    let biggest = frames.iter().map(|f| f.new_lines.len()).max().unwrap();
    assert!(
        biggest > 1000,
        "the burst should arrive in large frames, got {biggest}"
    );
}

#[test]
fn vim_uses_the_alternate_screen_and_restores_the_shell_screen() {
    let c = cast("vim");
    let s = at(&c, "vim-text");
    assert!(s.modes().alt_screen);
    assert!(s.contains("Hello from vim"));
    assert!(s.contains("wide: 界 世界"));
    assert!(s.contains("combining: e\u{301} n\u{303}"));
    assert!(s.saved_primary().is_some());
    let split = at(&c, "vim-split");
    assert!(split.contains("notes.txt"));
    let end = Replay::new(&c).final_screen();
    assert!(!end.modes().alt_screen);
}

#[test]
fn less_pages_searches_and_shows_help() {
    let c = cast("less");
    assert!(at(&c, "less-first-page").contains("row 1 of the less test file"));
    assert!(at(&c, "less-search").contains("row 160 "));
    assert!(at(&c, "less-help").contains("SUMMARY OF LESS COMMANDS"));
}

#[test]
fn claude_prompt_working_and_interrupt() {
    let c = cast("claude-prompt");
    let typed = at(&c, "prompt-typed");
    // The prompt marker is followed by a no-break space; matching treats it as a space.
    assert!(typed.contains("❯ Count from 1 to 10"));
    assert!(at(&c, "working").contains("esc to interrupt"));
    let r = at(&c, "response");
    assert!(!r.contains("esc to interrupt"));
    assert!(r.lines().iter().any(|l| l.trim() == "10"));
    assert!(at(&c, "interrupted").contains("Interrupted"));
    // While working, frames settle often: the spinner animates.
    let frames = Replay::new(&c).frames();
    let working = frames
        .iter()
        .filter(|f| f.screen.contains("esc to interrupt"))
        .count();
    assert!(working >= 3, "{working} working frames");
}

#[test]
fn claude_slash_menu_and_model_picker() {
    let c = cast("claude-slash");
    let m = at(&c, "slash-menu");
    assert!(m.contains("/add-dir"));
    assert!(at(&c, "slash-menu-filtered").contains("❯ /model"));
    let c = cast("claude-model");
    let p = at(&c, "model-picker");
    assert!(p.contains("Select model"));
    assert!(p.contains("Esc to cancel"));
    assert!(!at(&c, "model-picker-closed").contains("Select model"));
}

#[test]
fn codex_screens() {
    let c = cast("codex");
    let idle = at(&c, "idle-settled");
    assert!(idle.modes().alt_screen);
    assert_eq!(idle.modes().kitty.bits(), 7);
    assert!(idle.contains("Ask Codex to do anything"));
    assert!(at(&c, "shortcuts").contains("Keyboard shortcuts"));
    assert!(at(&c, "slash-menu").contains("/model"));
    assert!(at(&c, "model-picker").contains("Select Model and Effort"));
    // Codex draws every frame inside a synchronized update; those frames settle on its end.
    let frames = Replay::new(&c).frames();
    assert!(
        frames
            .iter()
            .filter(|f| f.reason == SettleReason::SyncEnd)
            .count()
            > 10
    );
}

#[test]
fn claude_title_shows_its_state() {
    let c = cast("claude-prompt");
    assert_eq!(at(&c, "idle").title(), Some("✳ Claude Code"));
    let working = at(&c, "working");
    let t = working.title().unwrap();
    assert!(t.starts_with('◐') || t.starts_with('◑'), "{t}");
    assert!(at(&c, "response").title().unwrap().starts_with('✳'));
}
