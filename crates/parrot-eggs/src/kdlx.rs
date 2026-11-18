//! Small helpers for reading KDL nodes.

use kdl::{KdlDocument, KdlNode, KdlValue};

/// Parses KDL text, with the error as one line.
pub fn parse(text: &str) -> Result<KdlDocument, String> {
    text.parse::<KdlDocument>().map_err(|e| {
        let detail = e
            .diagnostics
            .first()
            .map(|d| {
                let what = d.message.clone().unwrap_or_else(|| "invalid KDL".into());
                format!("{what} at byte {}", d.span.offset())
            })
            .unwrap_or_else(|| e.to_string());
        format!("not valid KDL: {detail}")
    })
}

/// The positional arguments of a node.
pub fn args(node: &KdlNode) -> impl Iterator<Item = &KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
}

/// The first positional argument as a string.
pub fn arg_str(node: &KdlNode) -> Option<&str> {
    args(node).next().and_then(KdlValue::as_string)
}

/// A property as a string.
pub fn prop_str<'a>(node: &'a KdlNode, name: &str) -> Option<&'a str> {
    node.entries()
        .iter()
        .find(|e| e.name().is_some_and(|n| n.value() == name))
        .and_then(|e| e.value().as_string())
}

/// A property as an integer.
pub fn prop_int(node: &KdlNode, name: &str) -> Option<i128> {
    node.entries()
        .iter()
        .find(|e| e.name().is_some_and(|n| n.value() == name))
        .and_then(|e| e.value().as_integer())
}

/// The child nodes, empty when there is no block.
pub fn children(node: &KdlNode) -> &[KdlNode] {
    node.children().map(KdlDocument::nodes).unwrap_or(&[])
}

/// A value written as text, for error messages and free-form arguments.
pub fn text(v: &KdlValue) -> String {
    match v {
        KdlValue::String(s) => s.clone(),
        other => other.to_string(),
    }
}
