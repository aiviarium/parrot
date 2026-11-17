//! A tree as indented text, one node per line.

use std::fmt::Write;

use super::{Node, Tree};

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Characters of text shown per node; 0 hides text.
    pub text_width: usize,
    /// Show which rule produced each node.
    pub sources: bool,
    /// Show style summaries.
    pub styles: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            text_width: 60,
            sources: true,
            styles: false,
        }
    }
}

pub fn render(tree: &Tree) -> String {
    render_with(tree, RenderOptions::default())
}

fn quote(s: &str, width: usize) -> String {
    let flat: String = s.replace('\n', "⏎");
    let mut out: String = flat.chars().take(width).collect();
    if flat.chars().count() > width {
        out.push('…');
    }
    format!("{out:?}")
}

pub fn render_with(tree: &Tree, opts: RenderOptions) -> String {
    let mut s = String::new();
    let _ = write!(s, "screen {}", tree.size);
    if let Some(t) = &tree.title {
        let _ = write!(s, " title={t:?}");
    }
    if tree.alt_screen {
        s.push_str(" alt-screen");
    }
    if let Some((r, c)) = tree.cursor {
        let _ = write!(s, " cursor={r},{c}");
    }
    s.push('\n');
    for c in &tree.root.children {
        node(&mut s, c, 1, opts);
    }
    s
}

fn node(s: &mut String, n: &Node, depth: usize, opts: RenderOptions) {
    let b = n.bounds;
    let _ = write!(
        s,
        "{:indent$}{} {} @{},{} {}x{}",
        "",
        n.role,
        n.id,
        b.row,
        b.col,
        b.cols,
        b.rows,
        indent = depth * 2
    );
    let states = n.state.names();
    if !states.is_empty() {
        let _ = write!(s, " {{{}}}", states.join(","));
    }
    if !n.tags.is_empty() {
        let _ = write!(s, " [{}]", n.tags.join(","));
    }
    if let Some(k) = &n.key {
        let _ = write!(s, " key={k}");
    }
    if let Some(k) = &n.shortcut {
        let _ = write!(s, " shortcut={k}");
    }
    if let Some(l) = &n.label {
        let _ = write!(s, " label={}", quote(l, opts.text_width.max(20)));
    }
    if let Some(v) = &n.value {
        let _ = write!(s, " value={}", quote(v, opts.text_width.max(20)));
    }
    if opts.text_width > 0 && !n.role.is_container() && n.label.as_deref() != Some(n.text.as_str()) {
        let _ = write!(s, " {}", quote(&n.text, opts.text_width));
    }
    if opts.styles {
        let st = &n.style;
        let mut parts = Vec::new();
        for (on, name) in [
            (st.bold, "bold"),
            (st.dim, "dim"),
            (st.italic, "italic"),
            (st.underline, "underline"),
            (st.reverse, "reverse"),
        ] {
            if on {
                parts.push(name.to_string());
            }
        }
        if let Some(fg) = &st.fg {
            parts.push(format!("fg={fg}"));
        }
        if let Some(bg) = &st.bg {
            parts.push(format!("bg={bg}"));
        }
        if !parts.is_empty() {
            let _ = write!(s, " ({})", parts.join(" "));
        }
    }
    if opts.sources {
        let _ = write!(s, "  ← {}", n.source);
    }
    s.push('\n');
    for c in &n.children {
        node(s, c, depth + 1, opts);
    }
}
