//! The profile file format, deserialised from TOML. The crate's README describes it.

use std::fmt;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::tree::Role;

/// A regular expression in a profile, compiled when the profile is read.
#[derive(Clone)]
pub struct Re(pub Regex);

impl fmt::Debug for Re {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for Re {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Regex::new(&s)
            .map(Re)
            .map_err(|e| serde::de::Error::custom(format!("bad regex {s:?}: {e}")))
    }
}

impl Serialize for Re {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0.as_str())
    }
}

/// A duration written as `5s`, `500ms`, `1.5s` or `2m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dur(pub Duration);

pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, "s"),
    };
    let n: f64 = num.trim().parse().ok()?;
    let secs = match unit {
        "ms" => n / 1000.0,
        "s" => n,
        "m" => n * 60.0,
        _ => return None,
    };
    (secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            S(String),
            N(f64),
        }
        match Raw::deserialize(d)? {
            Raw::S(s) => parse_duration(&s)
                .map(Dur)
                .ok_or_else(|| serde::de::Error::custom(format!("bad duration {s:?}"))),
            Raw::N(n) => Ok(Dur(Duration::from_secs_f64(n.max(0.0)))),
        }
    }
}

impl Serialize for Dur {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{}ms", self.0.as_millis()))
    }
}

/// One value or a list of them.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    pub fn as_slice(&self) -> &[T] {
        match self {
            OneOrMany::One(t) => std::slice::from_ref(t),
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    /// Starts in the top third of the screen.
    Top,
    /// Ends in the bottom third of the screen.
    Bottom,
}

/// Conditions on one node. All given keys must hold.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Matcher {
    pub role: Option<OneOrMany<Role>>,
    /// Has this tag (from an earlier rule).
    pub tag: Option<String>,
    /// The rule that produced it starts with this, such as `auto:box` or `auto:rule-section`.
    pub source: Option<String>,
    /// Drawn as a box.
    #[serde(rename = "box")]
    pub is_box: Option<bool>,
    pub contains_cursor: Option<bool>,
    pub focused: Option<bool>,
    pub selected: Option<bool>,
    pub busy: Option<bool>,
    pub position: Option<Position>,
    /// Regex over the node's text (rows joined with `\n`).
    pub text_matches: Option<Re>,
    pub text_contains: Option<String>,
    /// The text, without leading spaces, starts with this.
    pub starts_with: Option<String>,
    /// Regex over the first line.
    pub first_line_matches: Option<Re>,
    pub label_matches: Option<Re>,
    pub value_matches: Option<Re>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
    pub italic: Option<bool>,
    pub reverse: Option<bool>,
    /// `#rrggbb` or a palette index.
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub min_rows: Option<u16>,
    pub max_rows: Option<u16>,
    /// Starts at or below the bottom of the first node with this tag.
    pub below: Option<String>,
    /// Ends at or above the top of the first node with this tag.
    pub above: Option<String>,
    /// Has a descendant matching this.
    pub has: Option<Box<Matcher>>,
    pub not: Option<Box<Matcher>>,
    pub any: Option<Vec<Matcher>>,
}

/// Groups rows into a new node: from the first row matching `from` to the first row at or
/// after it matching `to`, or to the last non-blank row of the scope.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LinesSpec {
    pub from: Re,
    pub to: Option<Re>,
    /// To the last row at or after `from` matching this.
    pub through: Option<Re>,
    /// Search from the bottom: the last row matching `from`.
    #[serde(default)]
    pub last: bool,
}

/// Matches one line of text.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LineMatcher {
    /// The line, without leading spaces, starts with this.
    pub starts_with: Option<String>,
    pub matches: Option<Re>,
    /// Most of the line is reverse video.
    pub reverse: Option<bool>,
    pub bold: Option<bool>,
    pub fg: Option<String>,
}

/// Re-splits a node's rows into items.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ItemSpec {
    /// A row matching this starts an item; other non-blank rows continue the previous one.
    pub line_matches: Re,
    pub selected_when: Option<LineMatcher>,
    /// Capture group 1 is the item's label.
    pub label: Option<Re>,
    /// Capture group 1 is the item's key.
    pub key: Option<Re>,
    /// Rows to skip. Rows made only of rule characters are always skipped.
    pub ignore: Option<Re>,
}

/// A rule that tags nodes of the raw tree, and can change their role, group rows into a new
/// node, or re-split a node into items.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NodeRule {
    pub tag: String,
    /// Shown as the rule that produced the node; defaults to the tag.
    pub name: Option<String>,
    /// Sets the role of matched nodes.
    pub role: Option<Role>,
    #[serde(rename = "where")]
    pub matcher: Option<Matcher>,
    /// Only descendants of a node with this tag.
    pub inside: Option<String>,
    pub lines: Option<LinesSpec>,
    pub item: Option<ItemSpec>,
    /// Only the first match, in document order.
    #[serde(default)]
    pub first: bool,
    #[serde(default)]
    pub last: bool,
    /// Capture group 1 of this regex over the text becomes the label.
    pub label: Option<Re>,
}

/// A condition over the labelled tree, the screen and the tracker's memory. All given keys
/// must hold.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Cond {
    /// In a `[[state]]`: that state's rule holds. Elsewhere: the current state is this one
    /// (or one of these).
    pub state: Option<OneOrMany<String>>,
    /// Nodes with these tags exist.
    pub exists: Option<OneOrMany<String>>,
    pub not_exists: Option<OneOrMany<String>>,
    /// A node matching this exists.
    pub find: Option<Matcher>,
    /// Regex over the whole screen text (rows joined with `\n`).
    pub any_text_matches: Option<Re>,
    pub no_text_matches: Option<Re>,
    pub title_matches: Option<Re>,
    pub alt_screen: Option<bool>,
    pub cursor_visible: Option<bool>,
    /// The node with this tag holds the cursor.
    pub focused: Option<String>,
    /// With `contains` or `matches`: the text (an input's value, if it has one) of the node with
    /// this tag. `contains` ignores whitespace and line wrapping.
    pub text_in: Option<String>,
    pub contains: Option<String>,
    pub matches: Option<Re>,
    /// The node with this tag has an empty value (an input with nothing typed).
    pub empty: Option<String>,
    /// With `index` or `label`: which item is selected in the menu with this tag.
    pub selected_in: Option<String>,
    pub index: Option<IndexSpec>,
    pub label: Option<String>,
    /// In an action's `expect`: the node with this tag is gone, or its text differs from
    /// when the action started.
    pub changed: Option<String>,
    /// The named state has never been current in this session.
    pub until_seen: Option<String>,
    /// The named state has been current at some point.
    pub seen: Option<String>,
    pub not: Option<NotCond>,
    pub any: Option<Vec<Cond>>,
    pub all: Option<Vec<Cond>>,
    /// For `expect` and `wait`: how long to wait.
    pub within: Option<Dur>,
    /// For `require`, `expect` and `wait`: the condition must hold this long without a break
    /// before it counts, so a screen seen for one frame (a redraw, a dialog about to replace
    /// the prompt) is not acted on.
    pub hold: Option<Dur>,
}

/// `not = ["working", "idle"]` (none of these states) or `not = { ... }`.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum NotCond {
    States(Vec<String>),
    Cond(Box<Cond>),
}

/// An index given as a number or as a string (after variable substitution).
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum IndexSpec {
    N(i64),
    S(String),
}

impl IndexSpec {
    pub fn get(&self) -> Option<i64> {
        match self {
            IndexSpec::N(n) => Some(*n),
            IndexSpec::S(s) => s.trim().parse().ok(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct StateRule {
    pub name: String,
    pub when: Cond,
    pub description: Option<String>,
}

/// A named input sequence. `require`, `steps` and `expect` may use `$name` variables, which
/// are substituted before they are read.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ActionDef {
    pub name: String,
    pub description: Option<String>,
    #[schemars(with = "Option<serde_json::Value>")]
    pub require: Option<toml::Value>,
    #[serde(default)]
    #[schemars(with = "Vec<serde_json::Value>")]
    pub steps: Vec<toml::Value>,
    #[schemars(with = "Option<serde_json::Value>")]
    pub expect: Option<toml::Value>,
}

/// Names a function in the host program that receives the tree and returns tags.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct HookRule {
    pub name: String,
}

/// Rules for a range of CLI versions. Rules with the tag, state or action name of a base rule
/// replace it; others are added.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Variant {
    pub version: String,
    #[serde(default)]
    pub node: Vec<NodeRule>,
    #[serde(default)]
    pub state: Vec<StateRule>,
    #[serde(default)]
    pub action: Vec<ActionDef>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MatchSpec {
    pub command: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Meta {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "match")]
    pub matches: Option<MatchSpec>,
    /// Capture group 1 is the CLI's version, read from the screen.
    pub version_from_screen: Option<Re>,
    /// No state for this long marks the session `unrecognised`. Default 10s.
    pub unrecognised_after: Option<Dur>,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProfileFile {
    pub profile: Meta,
    #[serde(default)]
    pub node: Vec<NodeRule>,
    #[serde(default)]
    pub state: Vec<StateRule>,
    #[serde(default)]
    pub action: Vec<ActionDef>,
    #[serde(default)]
    pub hook: Vec<HookRule>,
    #[serde(default)]
    pub variant: Vec<Variant>,
}

/// One step of an action, after variable substitution.
#[derive(Clone, Debug)]
pub enum Step {
    /// Press keys by name, in order.
    Key(Vec<String>),
    Type(String),
    Paste(String),
    Select(SelectStep),
    Wait {
        cond: Box<Cond>,
        timeout: Duration,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SelectStep {
    /// Tag of the menu.
    #[serde(rename = "in")]
    pub menu: String,
    /// 0-based position.
    pub index: Option<IndexSpec>,
    /// Case-insensitive substring of the item's label or text.
    pub label: Option<String>,
    /// The item's key (`1`), its shortcut, or a substring of its label.
    pub option: Option<String>,
    /// Press Enter once the item is selected.
    #[serde(default)]
    pub confirm: bool,
    /// How long the selection must stay on the item before the step is done. Default 200ms:
    /// a program redrawing at startup can reset it.
    pub hold: Option<Dur>,
    pub timeout: Option<Dur>,
}

/// A string schema, for types read from strings.
fn string_schema(description: &str) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "description": description})
}

impl schemars::JsonSchema for Re {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Regex".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        string_schema("A regular expression (Rust regex syntax)")
    }
}

impl schemars::JsonSchema for Dur {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Duration".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A duration such as \"300ms\" or \"10s\", or seconds as a number",
            "type": ["string", "number"],
        })
    }
}
