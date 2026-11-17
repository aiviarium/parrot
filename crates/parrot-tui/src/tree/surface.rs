//! Generic surfaces: dialogs found by segmentation alone, with no profile, described so a UI
//! can render them as a card (text, options as buttons) and answer them.

use serde::{Deserialize, Serialize};

use super::{Node, Role, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SurfaceKind {
    /// A list of options, one of them selected.
    Choice,
    /// A text field.
    TextField,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceOption {
    pub index: usize,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    pub selected: bool,
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub disabled: bool,
    /// Keys that choose this option from the current selection: arrows, then Enter.
    pub keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceInput {
    pub id: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

/// A dialog as a UI would show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Surface {
    /// The dialog's node id.
    pub id: String,
    /// The menu's node id, for a choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu: Option<String>,
    pub kind: SurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The dialog's text without its options and hints.
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SurfaceOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<SurfaceInput>,
    /// Key hints shown in the dialog, such as `Esc to cancel`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    /// Tags from a profile, if one labelled the dialog.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Keys that move a menu's selection from `from` to `to` and confirm.
pub fn choose_keys(from: Option<usize>, to: usize) -> Vec<String> {
    let from = from.unwrap_or(0);
    let mut keys = Vec::new();
    let key = if to > from { "down" } else { "up" };
    for _ in 0..from.abs_diff(to) {
        keys.push(key.to_string());
    }
    keys.push("enter".into());
    keys
}

fn collect<'a>(n: &'a Node, roles: &[Role], out: &mut Vec<&'a Node>) {
    for c in &n.children {
        if roles.contains(&c.role) {
            out.push(c);
        }
        if c.role != Role::Dialog {
            collect(c, roles, out);
        }
    }
}

fn surface_of(d: &Node) -> Option<Surface> {
    let mut menus = Vec::new();
    collect(d, &[Role::Menu], &mut menus);
    let mut inputs = Vec::new();
    collect(d, &[Role::Input], &mut inputs);
    let mut hints = Vec::new();
    collect(d, &[Role::Hint], &mut hints);
    let mut texts = Vec::new();
    collect(d, &[Role::Text, Role::Heading], &mut texts);
    let text = texts
        .iter()
        .map(|t| t.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let (kind, menu, options, selected) = if let Some(m) = menus.first() {
        let sel = m.selected_index();
        let opts = m
            .items()
            .iter()
            .enumerate()
            .map(|(i, it)| SurfaceOption {
                index: i,
                label: it.display().to_string(),
                key: it.key.clone(),
                shortcut: it.shortcut.clone(),
                selected: it.state.selected,
                checked: it.state.checked,
                disabled: it.state.disabled,
                keys: choose_keys(sel, i),
            })
            .collect();
        (SurfaceKind::Choice, Some(m.id.clone()), opts, sel)
    } else if !inputs.is_empty() {
        (SurfaceKind::TextField, None, Vec::new(), None)
    } else {
        return None;
    };
    Some(Surface {
        id: d.id.clone(),
        menu,
        kind,
        title: d.label.clone(),
        text,
        options,
        selected,
        input: inputs.first().map(|i| SurfaceInput {
            id: i.id.clone(),
            value: i.value.clone().unwrap_or_default(),
            placeholder: i.label.clone(),
        }),
        hints: hints.iter().map(|h| h.text.trim().to_string()).collect(),
        tags: d.tags.clone(),
    })
}

/// Every dialog on the screen with options or a text field, top first.
pub fn surfaces(tree: &Tree) -> Vec<Surface> {
    let mut dialogs = Vec::new();
    if tree.root.role == Role::Dialog {
        dialogs.push(&tree.root);
    }
    collect(&tree.root, &[Role::Dialog], &mut dialogs);
    dialogs.into_iter().filter_map(surface_of).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use parrot_term::screen::{Screen, Size};

    #[test]
    fn a_dialog_with_choices_is_a_surface() {
        let rule = "─".repeat(40);
        let text =
            format!("log\n{rule}\n Allow this?\n ❯ 1. Yes\n   2. No\n\n Enter to confirm · Esc to cancel");
        let t = crate::tree::segment(&Screen::from_text(Size::new(7, 40), &text));
        let s = surfaces(&t);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, SurfaceKind::Choice);
        assert_eq!(s[0].text, "Allow this?");
        assert_eq!(s[0].options.len(), 2);
        assert_eq!(s[0].selected, Some(0));
        assert_eq!(s[0].options[1].keys, vec!["down", "enter"]);
        assert_eq!(s[0].hints, vec!["Enter to confirm · Esc to cancel"]);
    }
}
