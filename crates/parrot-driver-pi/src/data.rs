//! The driver data this crate reads: `options.kdl`, `levels.kdl` and `correlate.kdl` (spec 31).
//!
//! The runtime sets `PARROT_DRIVER_DATA` to the installed egg's `parrot` folder. Without it the copy
//! of the egg in this repository is compiled in, so a fresh install works offline. A file that is
//! missing or malformed keeps the built-in values for everything it would have set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use parrot_proto::option::PermissionLevel;

/// The files of one agent's driver data.
pub struct Sources {
    pub options: Option<String>,
    pub levels: Option<String>,
    pub correlate: Option<String>,
}

impl Sources {
    /// Reads `$PARROT_DRIVER_DATA`, falling back to texts compiled into the driver.
    pub fn load(builtin: fn() -> Sources) -> Sources {
        match std::env::var_os("PARROT_DRIVER_DATA").map(PathBuf::from) {
            Some(dir) => Sources {
                options: read(&dir, "options.kdl"),
                levels: read(&dir, "levels.kdl"),
                correlate: read(&dir, "correlate.kdl"),
            },
            None => builtin(),
        }
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

/// What the driver data says, in the shape this driver uses it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DriverData {
    /// The levels the agent lists, lowest first, as `levels.kdl` names them.
    pub levels: Vec<PermissionLevel>,
    /// Which level a tool of that name runs at.
    pub tools: BTreeMap<String, PermissionLevel>,
    /// Option labels, by option id.
    pub labels: BTreeMap<String, String>,
    /// Choices an option offers when the harness reports none, by option id.
    pub choices: BTreeMap<String, Vec<Choice>>,
    /// Which source wins for an item kind, from `correlate.kdl`.
    pub prefer: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub id: String,
    pub label: String,
}

impl DriverData {
    pub fn load(builtin: fn() -> Sources) -> Self {
        let sources = Sources::load(builtin);
        let mut data = DriverData::default();
        if let Some(text) = &sources.options {
            data.read_options(text);
        }
        if let Some(text) = &sources.levels {
            data.read_levels(text);
        }
        if let Some(text) = &sources.correlate {
            data.read_correlate(text);
        }
        data
    }

    /// The built-in values, used where a file does not say.
    pub fn fill(&mut self, other: DriverData) {
        if self.levels.is_empty() {
            self.levels = other.levels;
        }
        for (k, v) in other.tools {
            self.tools.entry(k).or_insert(v);
        }
        for (k, v) in other.labels {
            self.labels.entry(k).or_insert(v);
        }
        for (k, v) in other.choices {
            self.choices.entry(k).or_insert(v);
        }
        for (k, v) in other.prefer {
            self.prefer.entry(k).or_insert(v);
        }
    }

    pub fn label(&self, option: &str, fallback: &str) -> String {
        self.labels
            .get(option)
            .cloned()
            .unwrap_or_else(|| fallback.to_owned())
    }

    /// The level a tool runs at; an unknown tool counts as `host`, so a new tool can't slip under a
    /// ceiling (spec 15.2).
    pub fn tool_level(&self, tool: &str) -> PermissionLevel {
        self.tools.get(tool).copied().unwrap_or(PermissionLevel::Host)
    }

    /// Whether `correlate.kdl` lets the native source win for this item kind.
    pub fn native_wins(&self, kind: &str) -> bool {
        self.prefer.get(kind).map(|p| p == "native").unwrap_or(true)
    }

    fn read_options(&mut self, text: &str) {
        for node in parse(text) {
            if node.keyword != "option" {
                continue;
            }
            let Some(id) = node.name.clone() else { continue };
            if let Some(label) = node.attr("label") {
                self.labels.insert(id.clone(), label.to_owned());
            }
            let choices: Vec<Choice> = node
                .children
                .iter()
                .filter(|c| c.keyword == "choice")
                .filter_map(|c| {
                    let cid = c.name.clone()?;
                    let label = c.attr("label").unwrap_or(&cid).to_owned();
                    Some(Choice { id: cid, label })
                })
                .collect();
            if !choices.is_empty() {
                self.choices.insert(id, choices);
            }
        }
    }

    fn read_levels(&mut self, text: &str) {
        for node in parse(text) {
            if node.keyword != "level" {
                continue;
            }
            let Some(level) = node.name.as_deref().and_then(level_of) else {
                continue;
            };
            self.levels.push(level);
            for tool in node.children.iter().filter(|c| c.keyword == "tool") {
                if let Some(name) = &tool.name {
                    self.tools.insert(name.clone(), level);
                }
            }
        }
        self.levels.sort();
        self.levels.dedup();
    }

    fn read_correlate(&mut self, text: &str) {
        for node in parse(text) {
            if node.keyword != "kind" {
                continue;
            }
            let (Some(kind), Some(prefer)) = (
                node.name.clone(),
                node.children
                    .iter()
                    .find(|c| c.keyword == "prefer")
                    .and_then(|c| c.name.clone()),
            ) else {
                continue;
            };
            self.prefer.insert(kind, prefer);
        }
    }
}

fn level_of(name: &str) -> Option<PermissionLevel> {
    match name {
        "read" => Some(PermissionLevel::Read),
        "write" => Some(PermissionLevel::Write),
        "sandboxed" => Some(PermissionLevel::Sandboxed),
        "host" => Some(PermissionLevel::Host),
        _ => None,
    }
}

/// One KDL node: its keyword, optional quoted name, attributes and children.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub keyword: String,
    pub name: Option<String>,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// Parses the subset of KDL the driver data files use: nodes, quoted names, `key="value"`
/// attributes, `{ }` children, `;` or a newline between nodes, and `//` comments.
pub fn parse(text: &str) -> Vec<Node> {
    // Driver data has no `//` inside a value, so comments can go before tokenizing.
    let text: String = text
        .lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '\n' | '\r' | '\t' | ' ' | ';' => {
                chars.next();
            }
            '{' | '}' => {
                tokens.push(Token::Brace(c));
                chars.next();
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(next) = chars.next() {
                                s.push(match next {
                                    'n' => '\n',
                                    't' => '\t',
                                    other => other,
                                });
                            }
                        }
                        other => s.push(other),
                    }
                }
                tokens.push(Token::Str(s));
            }
            _ => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if matches!(c, '\n' | '\r' | '\t' | ' ' | ';' | '{' | '}' | '"') {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(Token::Word(s));
            }
        }
    }
    let mut at = 0;
    parse_nodes(&tokens, &mut at, false)
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Str(String),
    Brace(char),
}

fn parse_nodes(tokens: &[Token], at: &mut usize, nested: bool) -> Vec<Node> {
    let mut nodes = Vec::new();
    while *at < tokens.len() {
        match &tokens[*at] {
            Token::Brace('}') => {
                *at += 1;
                if nested {
                    return nodes;
                }
            }
            Token::Brace('{') => {
                // A brace with no node before it belongs to the last node already closed.
                *at += 1;
                parse_nodes(tokens, at, true);
            }
            Token::Word(keyword) => {
                let keyword = keyword.clone();
                *at += 1;
                let mut node = Node {
                    keyword,
                    name: None,
                    attrs: Vec::new(),
                    children: Vec::new(),
                };
                loop {
                    match tokens.get(*at) {
                        Some(Token::Str(s)) if node.name.is_none() => {
                            *at += 1;
                            node.name = Some(s.clone());
                        }
                        Some(Token::Str(s)) => {
                            *at += 1;
                            let _ = s;
                        }
                        Some(Token::Word(w)) => match w.split_once('=') {
                            Some((k, v)) => {
                                *at += 1;
                                let value = if v.is_empty() {
                                    match tokens.get(*at) {
                                        Some(Token::Str(s)) => {
                                            *at += 1;
                                            s.clone()
                                        }
                                        _ => String::new(),
                                    }
                                } else {
                                    v.trim_matches('"').to_owned()
                                };
                                node.attrs.push((k.to_owned(), value));
                            }
                            None => *at += 1,
                        },
                        Some(Token::Brace('{')) => {
                            *at += 1;
                            node.children = parse_nodes(tokens, at, true);
                            break;
                        }
                        _ => break,
                    }
                }
                nodes.push(node);
            }
            Token::Str(s) => {
                *at += 1;
                let _ = s;
            }
            Token::Brace(_) => {
                *at += 1;
            }
        }
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels() {
        let mut data = DriverData::default();
        data.read_levels(
            r#"
            // which tools each level allows
            level "read" { tool "Read"; tool "Grep"; }
            level "host" { tool "Bash"; }
            "#,
        );
        assert_eq!(data.levels, vec![PermissionLevel::Read, PermissionLevel::Host]);
        assert_eq!(data.tool_level("Read"), PermissionLevel::Read);
        assert_eq!(data.tool_level("Bash"), PermissionLevel::Host);
        assert_eq!(data.tool_level("Unknown"), PermissionLevel::Host);
    }

    #[test]
    fn parses_options_and_correlate() {
        let mut data = DriverData::default();
        data.read_options(
            r#"option "model" role="model" label="Model" applies="now" {
                 choice "default" label="Default"
               }"#,
        );
        data.read_correlate(r#"kind "tool" { match "ref"; prefer "native" }"#);
        assert_eq!(data.label("model", "x"), "Model");
        assert_eq!(data.choices["model"][0].label, "Default");
        assert!(data.native_wins("tool"));
    }
}
