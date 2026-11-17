//! A labelled tree over a terminal screen: in effect an accessibility tree for terminal UIs.
//!
//! - [`segment()`] builds the raw tree from a [`Screen`] with no knowledge of any program:
//!   boxes, horizontal rules and the regions between them, background fills, vertical splits,
//!   blank-row gaps, option lists with a selection marker, aligned columns, key-hint lines,
//!   progress lines and the input line holding the cursor.
//! - [`assign_ids`] gives nodes ids that are stable across frames, by matching each node with
//!   the most similar node of the previous tree.
//! - [`diff()`] lists what changed between two trees.
//! - [`render()`] prints a tree as indented text, for the inspector and for error messages.
//!
//! Profiles (`crate::profile`) then tag and correct the raw tree for one program.

mod diff;
mod ids;
mod render;
mod segment;
mod surface;

use serde::{Deserialize, Serialize};

pub use crate::locator::Rect;
pub use diff::{NodeChange, NodeRef, TreeDiff, diff};
pub use ids::assign_ids;
use parrot_term::screen::{Attrs, Color, Screen, Size, Style};
pub use render::{RenderOptions, render, render_with};
pub use segment::segment;
pub use surface::{Surface, SurfaceInput, SurfaceKind, SurfaceOption, choose_keys, surfaces};

/// What a node is, as far as interaction goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Screen,
    Pane,
    Dialog,
    Input,
    Menu,
    Menuitem,
    List,
    Listitem,
    Table,
    Row,
    Text,
    Heading,
    Status,
    Progress,
    Hint,
    Scroll,
    Separator,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Screen => "screen",
            Role::Pane => "pane",
            Role::Dialog => "dialog",
            Role::Input => "input",
            Role::Menu => "menu",
            Role::Menuitem => "menuitem",
            Role::List => "list",
            Role::Listitem => "listitem",
            Role::Table => "table",
            Role::Row => "row",
            Role::Text => "text",
            Role::Heading => "heading",
            Role::Status => "status",
            Role::Progress => "progress",
            Role::Hint => "hint",
            Role::Scroll => "scroll",
            Role::Separator => "separator",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }

    /// Roles that hold other nodes rather than text of their own.
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            Role::Screen | Role::Pane | Role::Dialog | Role::Menu | Role::List | Role::Table
        )
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Interaction state of a node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeState {
    /// The visible cursor is in this node.
    #[serde(default, skip_serializing_if = "is_false")]
    pub focused: bool,
    /// The selected item of a menu: marked with a glyph such as `❯` or highlighted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub selected: bool,
    /// Drawn dim among siblings that are not.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    /// A spinner or a running timer.
    #[serde(default, skip_serializing_if = "is_false")]
    pub busy: bool,
    /// Ticked: `✔`, `[x]`, `◉`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub checked: bool,
}

impl NodeState {
    pub fn names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (on, name) in [
            (self.focused, "focused"),
            (self.selected, "selected"),
            (self.disabled, "disabled"),
            (self.busy, "busy"),
            (self.checked, "checked"),
        ] {
            if on {
                v.push(name);
            }
        }
        v
    }
}

/// The dominant style of a node's text, for style matchers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextStyle {
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dim: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reverse: bool,
    /// `#rrggbb` or a palette index; absent for the default colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
}

pub(crate) fn color_name(c: Color) -> Option<String> {
    match c {
        Color::Default => None,
        Color::Indexed(i) => Some(i.to_string()),
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
    }
}

impl From<Style> for TextStyle {
    fn from(st: Style) -> Self {
        TextStyle {
            bold: st.attrs.contains(Attrs::BOLD),
            dim: st.attrs.contains(Attrs::DIM),
            italic: st.attrs.contains(Attrs::ITALIC),
            underline: st.attrs.intersects(Attrs::ANY_UNDERLINE),
            reverse: st.attrs.contains(Attrs::REVERSE),
            fg: color_name(st.fg),
            bg: color_name(st.bg),
        }
    }
}

/// One node of the tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// Stable across frames once [`assign_ids`] ran; `n<k>` before.
    pub id: String,
    pub role: Role,
    /// From a title, a rule, a heading, or nearby text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The text in the node's bounds, rows joined with `\n`.
    pub text: String,
    pub bounds: Rect,
    #[serde(default)]
    pub state: NodeState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// Assigned by profile rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Which rule produced the node: `auto:<rule>` for segmentation, `profile:<tag>` for a
    /// profile rule.
    pub source: String,
    /// For an input: the text typed into it, without the prompt glyph or a placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// For an item: the number or letter shown before it (`1` in `1. Yes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// For an item: a shortcut shown after it (`y` in `Yes, proceed (y)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    /// For a row or an item laid out in aligned columns: the text of each column.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    #[serde(default)]
    pub style: TextStyle,
}

impl Node {
    pub fn new(role: Role, bounds: Rect, text: impl Into<String>, source: &str) -> Node {
        Node {
            id: String::new(),
            role,
            label: None,
            text: text.into(),
            bounds,
            state: NodeState::default(),
            children: Vec::new(),
            tags: Vec::new(),
            source: source.to_string(),
            value: None,
            key: None,
            shortcut: None,
            columns: Vec::new(),
            style: TextStyle::default(),
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    pub fn add_tag(&mut self, tag: &str) {
        if !self.has_tag(tag) {
            self.tags.push(tag.to_string());
        }
    }

    /// The first line of the text.
    pub fn first_line(&self) -> &str {
        self.text.lines().next().unwrap_or("")
    }

    /// This node and all its descendants, depth first.
    pub fn walk(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        fn go<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
            out.push(n);
            for c in &n.children {
                go(c, out);
            }
        }
        go(self, &mut out);
        out
    }

    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Node)) {
        f(self);
        for c in &mut self.children {
            c.walk_mut(f);
        }
    }

    /// The items of a menu or list: direct children with an item role.
    pub fn items(&self) -> Vec<&Node> {
        self.children
            .iter()
            .filter(|c| matches!(c.role, Role::Menuitem | Role::Listitem | Role::Row))
            .collect()
    }

    /// Index of the selected item, if any.
    pub fn selected_index(&self) -> Option<usize> {
        self.items().iter().position(|c| c.state.selected)
    }

    /// The text an item stands for: its label if it has one, otherwise its text.
    pub fn display(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.text)
    }
}

/// A tree for one screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tree {
    pub root: Node,
    pub size: Size,
    /// The cursor, when it is visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<(u16, u16)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub alt_screen: bool,
}

impl Tree {
    /// Every node, depth first.
    pub fn nodes(&self) -> Vec<&Node> {
        self.root.walk()
    }

    pub fn by_id(&self, id: &str) -> Option<&Node> {
        self.nodes().into_iter().find(|n| n.id == id)
    }

    /// Nodes with this tag, top first.
    pub fn tagged(&self, tag: &str) -> Vec<&Node> {
        self.nodes().into_iter().filter(|n| n.has_tag(tag)).collect()
    }

    /// The first node with this tag.
    pub fn find_tag(&self, tag: &str) -> Option<&Node> {
        self.nodes().into_iter().find(|n| n.has_tag(tag))
    }

    pub fn with_role(&self, role: Role) -> Vec<&Node> {
        self.nodes().into_iter().filter(|n| n.role == role).collect()
    }

    /// The chain of nodes from the root to the node with this id.
    pub fn path_to(&self, id: &str) -> Vec<&Node> {
        fn go<'a>(n: &'a Node, id: &str, path: &mut Vec<&'a Node>) -> bool {
            path.push(n);
            if n.id == id {
                return true;
            }
            for c in &n.children {
                if go(c, id, path) {
                    return true;
                }
            }
            path.pop();
            false
        }
        let mut path = Vec::new();
        go(&self.root, id, &mut path);
        path
    }
}

/// Builds the raw tree for a screen and gives it ids that follow `previous`.
pub fn build(screen: &Screen, previous: Option<&Tree>, counter: &mut u64) -> Tree {
    let mut tree = segment(screen);
    assign_ids(previous, &mut tree, counter);
    tree
}
