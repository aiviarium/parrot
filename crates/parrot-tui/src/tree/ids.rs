//! Stable node ids: each node takes the id of the most similar unused node of the previous
//! tree with the same role, or a new one.

use std::collections::HashSet;

use super::{Node, Rect, Tree};

struct Prev<'a> {
    node: &'a Node,
    parent: String,
}

fn iou(a: Rect, b: Rect) -> f64 {
    let Some(i) = a.intersect(&b) else {
        return 0.0;
    };
    let ia = i.rows as f64 * i.cols as f64;
    let ua = a.rows as f64 * a.cols as f64 + b.rows as f64 * b.cols as f64 - ia;
    if ua <= 0.0 { 0.0 } else { ia / ua }
}

fn score(p: &Prev, n: &Node, parent: &str) -> Option<f64> {
    let shared_tag = n.tags.iter().any(|t| p.node.has_tag(t));
    let overlap = iou(p.node.bounds, n.bounds);
    let same_label = n.label.is_some() && n.label == p.node.label;
    let same_line = !n.first_line().trim().is_empty() && n.first_line() == p.node.first_line();
    let same_key = n.key.is_some() && n.key == p.node.key;
    let same_parent = p.parent == parent;
    if !(shared_tag || overlap > 0.2 || same_label || same_line || (same_key && same_parent)) {
        return None;
    }
    let mut s = 40.0 * overlap;
    if shared_tag {
        s += 100.0;
    }
    if same_parent {
        s += 15.0;
    }
    if same_label {
        s += 25.0;
    }
    if same_line {
        s += 25.0;
    }
    if same_key {
        s += 10.0;
    }
    if p.node.bounds.row == n.bounds.row {
        s += 5.0;
    }
    (s >= 20.0).then_some(s)
}

/// Gives every node of `tree` an id: the id of the best matching node of `previous`, or a new
/// `<role>-<n>` from `counter`. The root is always `screen`.
pub fn assign_ids(previous: Option<&Tree>, tree: &mut Tree, counter: &mut u64) {
    let mut prev = Vec::new();
    if let Some(p) = previous {
        fn flat<'a>(n: &'a Node, parent: &str, out: &mut Vec<Prev<'a>>) {
            out.push(Prev {
                node: n,
                parent: parent.to_string(),
            });
            for c in &n.children {
                flat(c, &n.id, out);
            }
        }
        flat(&p.root, "", &mut prev);
    }
    let mut used: HashSet<String> = HashSet::new();
    fn go(
        n: &mut Node,
        parent: &str,
        prev: &[Prev],
        used: &mut HashSet<String>,
        counter: &mut u64,
        root: bool,
    ) {
        if root {
            n.id = "screen".into();
        } else {
            let best = prev
                .iter()
                .filter(|p| p.node.role == n.role && !used.contains(&p.node.id))
                .filter_map(|p| score(p, n, parent).map(|s| (s, p)))
                .max_by(|a, b| a.0.total_cmp(&b.0));
            n.id = match best {
                Some((_, p)) => p.node.id.clone(),
                None => {
                    *counter += 1;
                    format!("{}-{}", n.role, counter)
                }
            };
        }
        used.insert(n.id.clone());
        let id = n.id.clone();
        for c in &mut n.children {
            go(c, &id, prev, used, counter, false);
        }
    }
    go(&mut tree.root, "", &prev, &mut used, counter, true);
}
