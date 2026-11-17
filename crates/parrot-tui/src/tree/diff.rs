//! Differences between two trees with stable ids.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{Node, NodeState, Rect, Role, Tree};

/// A node named in a diff.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRef {
    pub id: String,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub text: String,
    pub bounds: Rect,
}

impl From<&Node> for NodeRef {
    fn from(n: &Node) -> Self {
        NodeRef {
            id: n.id.clone(),
            role: n.role,
            tags: n.tags.clone(),
            label: n.label.clone(),
            text: n.text.clone(),
            bounds: n.bounds,
        }
    }
}

/// A node present in both trees whose own fields changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeChange {
    pub id: String,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Which fields changed: `text`, `label`, `bounds`, `state`, `tags`, `value`.
    pub fields: Vec<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub state: NodeState,
    pub bounds: Rect,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeDiff {
    pub added: Vec<NodeRef>,
    pub removed: Vec<NodeRef>,
    pub changed: Vec<NodeChange>,
}

impl TreeDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// What changed from `old` to `new`, by node id. Containers report text changes only through
/// their children.
pub fn diff(old: &Tree, new: &Tree) -> TreeDiff {
    let olds: HashMap<&str, &Node> = old.nodes().into_iter().map(|n| (n.id.as_str(), n)).collect();
    let news: HashMap<&str, &Node> = new.nodes().into_iter().map(|n| (n.id.as_str(), n)).collect();
    let mut d = TreeDiff::default();
    for n in new.nodes() {
        match olds.get(n.id.as_str()) {
            None => d.added.push(n.into()),
            Some(o) => {
                let mut fields = Vec::new();
                if !n.role.is_container() && o.text != n.text {
                    fields.push("text".to_string());
                }
                if o.label != n.label {
                    fields.push("label".into());
                }
                if o.bounds != n.bounds {
                    fields.push("bounds".into());
                }
                if o.state != n.state {
                    fields.push("state".into());
                }
                if o.tags != n.tags {
                    fields.push("tags".into());
                }
                if o.value != n.value {
                    fields.push("value".into());
                }
                if !fields.is_empty() {
                    d.changed.push(NodeChange {
                        id: n.id.clone(),
                        role: n.role,
                        tags: n.tags.clone(),
                        fields,
                        text: n.text.clone(),
                        label: n.label.clone(),
                        value: n.value.clone(),
                        state: n.state,
                        bounds: n.bounds,
                    });
                }
            }
        }
    }
    for o in old.nodes() {
        if !news.contains_key(o.id.as_str()) {
            d.removed.push(o.into());
        }
    }
    d
}
