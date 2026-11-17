//! Locator checks over recorded terminal screens.
use parrot_term::{Cast, KittyFlags, MouseMode, Replay, Screen, SettleReason, Size};

use parrot_tui::Locator;

fn cast(name: &str) -> Cast {
    Cast::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/casts")
            .join(format!("{name}.cast")),
    )
    .unwrap()
}

fn at(cast: &Cast, marker: &str) -> Screen {
    let t = cast.markers().into_iter().find(|(_, m)| m == marker).unwrap().0;
    Replay::new(cast).screen_at(t)
}

#[test]
fn top_and_nano() {
    let t = at(&cast("top"), "top-main");
    assert!(t.contains("PID USER"));
    let n = cast("nano");
    assert!(at(&n, "nano-text").contains("Hello from nano"));
    assert!(at(&n, "nano-save-prompt").contains("Write to File"));
}

#[test]
fn claude_startup_trust_dialog_then_fullscreen() {
    let c = cast("claude-startup");
    let trust = at(&c, "trust-dialog");
    assert!(!trust.modes().alt_screen, "the trust dialog is drawn inline");
    let no = Locator::contains("No, exit").find(&trust).unwrap();
    assert_eq!(trust.cell(no.row, no.col - 2).unwrap().text(), "❯");
    let yes = at(&c, "trust-yes-selected");
    assert!(yes.contains("❯ Yes, I trust this folder"));
    let idle = at(&c, "idle");
    assert!(idle.modes().alt_screen);
    assert_eq!(idle.modes().mouse, MouseMode::Motion);
    assert_eq!(
        idle.modes().kitty,
        KittyFlags::DISAMBIGUATE | KittyFlags::REPORT_ALTERNATE_KEYS
    );
    assert!(idle.contains("manual mode on"));
    assert_eq!(at(&c, "resized-80x24").size(), Size::new(24, 80));
    let big = at(&c, "resized-120x36");
    assert_eq!(big.size(), Size::new(36, 120));
    assert!(big.contains("manual mode on"));
}

#[test]
fn claude_permission_dialog() {
    let c = cast("claude-permission");
    let p = at(&c, "permission-prompt");
    assert!(p.contains("Do you want to proceed?"));
    let yes = Locator::contains("1. Yes").find(&p).unwrap();
    assert!(p.row(yes.row).unwrap().text().trim_start().starts_with('❯'));
    assert!(p.contains("Esc to cancel"));
    let moved = at(&c, "permission-option-2");
    let two = Locator::contains("2. Yes, and").find(&moved).unwrap();
    assert!(moved.row(two.row).unwrap().text().trim_start().starts_with('❯'));
    assert!(!at(&c, "approved").contains("Do you want to proceed?"));
    assert!(at(&c, "rejected").contains("Interrupted"));
    // The dialog never goes quiet (a bullet blinks), so frames there come from the max delay or
    // the blink pauses, not from a long silence.
    let frames = Replay::new(&c).frames();
    assert!(
        frames
            .iter()
            .any(|f| f.screen.contains("Do you want to proceed?"))
    );
    assert!(
        frames.iter().all(|f| f.reason != SettleReason::SyncEnd),
        "no synchronized updates"
    );
}
