//! Reading a dialog off the labelled tree: its text and the choices a client can answer with.

use parrot_term::screen::Screen;
use parrot_tui::tree::{choose_keys, surfaces};
use parrot_tui::{Role, Tree};
use serde::Serialize;

/// A dialog as the driver offers it in a request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dialog {
    /// The generic surface's id, when segmentation found one; `Automation::choose` answers it.
    pub surface: Option<String>,
    pub title: Option<String>,
    pub text: String,
    pub options: Vec<DialogOption>,
    /// Key hints, such as `Esc to cancel`.
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DialogOption {
    pub index: usize,
    /// The choice id: the option's number or letter when it shows one, else `option-<index>`.
    pub id: String,
    pub label: String,
    /// The number or letter shown before the option.
    pub key: Option<String>,
    /// Keys that pick this option from the current selection.
    pub keys: Vec<String>,
}

/// The dialog on screen: a profile-tagged `choices` menu first, then the first generic surface
/// with options, then the first one at all.
pub fn read(tree: &Tree) -> Option<Dialog> {
    if let Some(menu) = tree.find_tag("choices").filter(|m| m.role == Role::Menu) {
        let items = menu.items();
        let selected = menu.selected_index();
        let parent = tree
            .path_to(&menu.id)
            .into_iter()
            .rev()
            .find(|n| n.role == Role::Dialog || n.tags.iter().any(|t| t != "choices"));
        let surface = surfaces(tree)
            .into_iter()
            .find(|s| s.menu.as_deref() == Some(menu.id.as_str()));
        let text = surface
            .as_ref()
            .map(|s| s.text.clone())
            .or_else(|| parent.map(|p| p.text.clone()))
            .unwrap_or_default();
        return Some(Dialog {
            surface: surface.as_ref().map(|s| s.id.clone()),
            title: surface.as_ref().and_then(|s| s.title.clone()),
            text,
            options: items
                .iter()
                .enumerate()
                .map(|(i, it)| DialogOption {
                    index: i,
                    id: choice_id(it.key.as_deref(), i),
                    label: it.display().to_string(),
                    key: it.key.clone(),
                    keys: choose_keys(selected, i),
                })
                .collect(),
            hints: surface.map(|s| s.hints).unwrap_or_default(),
        });
    }
    let all = surfaces(tree);
    let s = all
        .iter()
        .find(|s| !s.options.is_empty())
        .or_else(|| all.first())?
        .clone();
    Some(Dialog {
        surface: Some(s.id),
        title: s.title,
        text: s.text,
        options: s
            .options
            .into_iter()
            .map(|o| DialogOption {
                index: o.index,
                id: choice_id(o.key.as_deref(), o.index),
                label: o.label,
                key: o.key,
                keys: o.keys,
            })
            .collect(),
        hints: s.hints,
    })
}

fn choice_id(key: Option<&str>, index: usize) -> String {
    key.map(str::to_owned)
        .unwrap_or_else(|| format!("option-{index}"))
}

/// The screen's text, trailing blank rows dropped.
pub fn screen_text(screen: &Screen) -> String {
    screen.text()
}

#[cfg(test)]
mod tests {
    use super::*;
    use parrot_term::cast::{Cast, Replay};
    use parrot_tui::{Profile, Tracker};

    fn fixture(path: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    }

    /// The Claude Code permission prompt, replayed from its cast through the live profile.
    #[test]
    fn a_claude_approval_reads_as_choices() {
        let profile = Profile::load(fixture("packages/parrot-agents/tui/claude-code.toml")).unwrap();
        let cast = Cast::load(fixture("crates/parrot-tui/fixtures/casts/claude-permission.cast")).unwrap();
        let mut tracker = Tracker::new(Some(profile), Default::default());
        let mut found = None;
        for f in Replay::new(&cast).frames() {
            tracker.update(f.time, &f.screen);
            if tracker.state() == Some("awaiting-approval") {
                found = read(tracker.tree().unwrap());
                break;
            }
        }
        let d = found.expect("the cast reaches the approval");
        assert!(d.options.len() >= 2, "{d:?}");
        assert_eq!(d.options[0].id, "1");
        assert!(d.options[0].label.starts_with("Yes"), "{d:?}");
        assert!(d.options.iter().any(|o| o.label.starts_with("No")), "{d:?}");
        assert_eq!(d.options[1].keys, ["down", "enter"]);
    }
}
