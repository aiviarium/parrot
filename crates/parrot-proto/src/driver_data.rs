//! Driver data (spec 31, Appendix E4): the KDL files in an egg's `parrot` folder, read as typed
//! data.
//!
//! A KDL document maps to JSON before it is read into the types here, so each file has a JSON
//! Schema. [`to_json`] does the mapping:
//!
//! | KDL | JSON |
//! | --- | --- |
//! | `key="v"` on a node | the field `key` of its object |
//! | a node with only arguments | the argument, or an array of them when there are several |
//! | a node with properties or children | an object; its first argument becomes `name` |
//! | children | fields of the object, grouped by node name ([`Children`] says when a group is an array) |
//! | `(array)node` | always an array element, even when the node appears once |

use std::collections::BTreeMap;
use std::path::Path;

use kdl::{KdlDocument, KdlNode, KdlValue};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::open::Open;
use crate::option::{Applies, ConfigOption, DependsOn, OptionRole, PermissionLevel};

/// The variable naming the installed egg's `parrot` folder.
pub const DRIVER_DATA_ENV: &str = "PARROT_DRIVER_DATA";

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("{file}: {message}")]
    Parse { file: String, message: String },
    #[error("{file}: {0}", file = .1)]
    Io(std::io::Error, String),
}

/// How children map to JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Children {
    /// Every group of children is an array, so a schema can say `Vec` everywhere.
    Lists,
    /// A child that appears once is a value; repeated or `(array)` children are an array.
    Grouped,
}

/// A KDL document as a JSON object.
pub fn to_json(text: &str, children: Children) -> Result<Value, String> {
    let doc: KdlDocument = text.parse().map_err(|e: kdl::KdlError| {
        let at = e
            .diagnostics
            .first()
            .map(|d| {
                let line = text[..d.span.offset().min(text.len())].lines().count().max(1);
                format!(" at line {line}: {}", d.message.clone().unwrap_or_default())
            })
            .unwrap_or_default();
        format!("invalid KDL{at}")
    })?;
    Ok(Value::Object(object(doc.nodes(), children)))
}

fn scalar(v: &KdlValue) -> Value {
    match v {
        KdlValue::String(s) => Value::String(s.clone()),
        KdlValue::Integer(i) => i64::try_from(*i).map_or_else(|_| json!(*i as f64), |i| json!(i)),
        KdlValue::Float(f) => json!(f),
        KdlValue::Bool(b) => Value::Bool(*b),
        KdlValue::Null => Value::Null,
    }
}

fn object(nodes: &[KdlNode], mode: Children) -> Map<String, Value> {
    let mut groups: Vec<(String, Vec<Value>, bool)> = Vec::new();
    for n in nodes {
        let name = n.name().value().to_owned();
        let forced = n.ty().is_some_and(|t| t.value() == "array");
        let value = node_value(n, mode);
        match groups.iter_mut().find(|g| g.0 == name) {
            Some(g) => {
                g.1.extend(value);
                g.2 |= forced;
            }
            None => groups.push((name, value.into_iter().collect(), forced)),
        }
    }
    let mut out = Map::new();
    for (name, mut values, forced) in groups {
        let v = if mode == Children::Lists || forced || values.len() != 1 {
            Value::Array(values)
        } else {
            values.pop().unwrap_or(Value::Null)
        };
        out.insert(name, v);
    }
    out
}

/// A node's value; `None` for an empty `(array)` marker.
fn node_value(n: &KdlNode, mode: Children) -> Option<Value> {
    let args: Vec<Value> = n
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| scalar(e.value()))
        .collect();
    let props: Vec<(String, Value)> = n
        .entries()
        .iter()
        .filter_map(|e| e.name().map(|k| (k.value().to_owned(), scalar(e.value()))))
        .collect();
    let kids = n.children().map(|d| d.nodes()).unwrap_or(&[]);
    if props.is_empty() && kids.is_empty() {
        return match args.len() {
            0 if n.ty().is_some_and(|t| t.value() == "array") => None,
            0 => Some(Value::Object(Map::new())),
            1 => args.into_iter().next(),
            _ => Some(Value::Array(args)),
        };
    }
    let mut obj = Map::new();
    match args.len() {
        0 => {}
        1 => {
            obj.insert("name".into(), args.into_iter().next().unwrap_or(Value::Null));
        }
        _ => {
            obj.insert("args".into(), Value::Array(args));
        }
    }
    for (k, v) in props {
        obj.insert(k, v);
    }
    obj.extend(object(kids, mode));
    Some(Value::Object(obj))
}

/// Reads one driver data file of type `T` (children as [`Children::Lists`]).
pub fn parse<T: DeserializeOwned>(file: &str, text: &str) -> Result<T, DataError> {
    let err = |message: String| DataError::Parse {
        file: file.to_owned(),
        message,
    };
    let v = to_json(text, Children::Lists).map_err(err)?;
    serde_json::from_value(v).map_err(|e| err(e.to_string()))
}

/// The text of `file` from `$PARROT_DRIVER_DATA`, else `builtin`, the copy compiled into the
/// driver. Returns where it came from too, for notices.
pub fn read(file: &str, builtin: &str) -> Result<(String, DataSource), DataError> {
    if let Some(dir) = std::env::var_os(DRIVER_DATA_ENV) {
        let path = Path::new(&dir).join(file);
        if path.exists() {
            let text =
                std::fs::read_to_string(&path).map_err(|e| DataError::Io(e, path.display().to_string()))?;
            return Ok((text, DataSource::Egg));
        }
    }
    Ok((builtin.to_owned(), DataSource::Builtin))
}

/// Where a data file was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSource {
    Egg,
    Builtin,
}

/// The compiled-in copies of an egg's data files.
#[derive(Debug, Clone, Copy, Default)]
pub struct Builtin {
    pub driver: &'static str,
    pub options: &'static str,
    pub levels: &'static str,
    pub correlate: &'static str,
}

/// An egg's data, loaded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DriverData {
    /// `data-version` from `driver.kdl`; frames carry it.
    pub version: String,
    pub options: OptionsFile,
    pub levels: LevelsFile,
    pub correlate: CorrelateFile,
}

impl DriverData {
    /// Reads every file from `$PARROT_DRIVER_DATA`, each falling back to its compiled-in copy.
    pub fn load(builtin: Builtin) -> Result<Self, DataError> {
        let (driver, _) = read("driver.kdl", builtin.driver)?;
        let (options, _) = read("options.kdl", builtin.options)?;
        let (levels, _) = read("levels.kdl", builtin.levels)?;
        let (correlate, _) = read("correlate.kdl", builtin.correlate)?;
        Self::parse(&driver, &options, &levels, &correlate)
    }

    /// Reads the compiled-in copies only.
    pub fn builtin(builtin: Builtin) -> Result<Self, DataError> {
        Self::parse(builtin.driver, builtin.options, builtin.levels, builtin.correlate)
    }

    fn parse(driver: &str, options: &str, levels: &str, correlate: &str) -> Result<Self, DataError> {
        let version = to_json(driver, Children::Lists)
            .ok()
            .and_then(|v| v["data-version"].get(0).cloned())
            .and_then(|v| match v {
                Value::String(s) => Some(s),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "1".into());
        Ok(DriverData {
            version,
            options: parse("options.kdl", options)?,
            levels: parse("levels.kdl", levels)?,
            correlate: parse("correlate.kdl", correlate)?,
        })
    }
}

// ---- options.kdl ----

/// `options.kdl`: options the harness doesn't report itself (spec 15.1, 31).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OptionsFile {
    #[serde(default)]
    pub option: Vec<OptionData>,
}

/// `option "<id>" role= label= applies= default= { choice ...; depends-on ... }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OptionData {
    /// The option id.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `start`, `now` or `next-turn`; `now` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// The JSON type of a free value, such as `string`, when there are no choices.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    /// The harness's own name for the option, such as a config key or a flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The harness reports the choices at runtime; these are the fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<bool>,
    #[serde(default)]
    pub choice: Vec<ChoiceData>,
    #[serde(default)]
    pub depends_on: Vec<DependsOnData>,
}

/// `choice "<value>" label= tags=`, or a bare `choice "<value>"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ChoiceData {
    Bare(String),
    Full(ChoiceFull),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ChoiceFull {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Semantic tags, separated by spaces: `plan`, `bypass`, `ask`, `read-only` and so on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<String>,
    /// The harness's own value, when it differs from `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
}

impl ChoiceData {
    pub fn value(&self) -> &str {
        match self {
            ChoiceData::Bare(s) => s,
            ChoiceData::Full(c) => &c.name,
        }
    }

    pub fn tags(&self) -> Vec<&str> {
        match self {
            ChoiceData::Bare(_) => vec![],
            ChoiceData::Full(c) => c.tags.as_deref().unwrap_or("").split_whitespace().collect(),
        }
    }

    /// The value the harness takes for this choice.
    pub fn harness(&self) -> &str {
        match self {
            ChoiceData::Full(ChoiceFull { harness: Some(h), .. }) => h,
            _ => self.value(),
        }
    }

    fn schema(&self) -> Value {
        match self {
            ChoiceData::Bare(s) => json!({"const": s, "title": s}),
            ChoiceData::Full(c) => {
                let mut m = Map::new();
                m.insert("const".into(), json!(c.name));
                m.insert("title".into(), json!(c.label.as_deref().unwrap_or(&c.name)));
                if let Some(d) = &c.description {
                    m.insert("description".into(), json!(d));
                }
                let tags = self.tags();
                if !tags.is_empty() {
                    m.insert("x-tags".into(), json!(tags));
                }
                Value::Object(m)
            }
        }
    }
}

/// `depends-on "<option>" { when "<value>" { choice ... } }`; `when "*"` matches any other value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DependsOnData {
    pub name: String,
    #[serde(default)]
    pub when: Vec<WhenChoices>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WhenChoices {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default)]
    pub choice: Vec<ChoiceData>,
}

fn choices_schema(choices: &[ChoiceData], ty: Option<&str>) -> Value {
    if choices.is_empty() {
        json!({"type": ty.unwrap_or("string")})
    } else {
        json!({"oneOf": choices.iter().map(ChoiceData::schema).collect::<Vec<_>>()})
    }
}

impl OptionsFile {
    pub fn get(&self, id: &str) -> Option<&OptionData> {
        self.option.iter().find(|o| o.name == id)
    }

    /// The first option with `role`.
    pub fn by_role(&self, role: &str) -> Option<&OptionData> {
        self.option.iter().find(|o| o.role.as_deref() == Some(role))
    }
}

impl OptionData {
    pub fn applies(&self) -> Applies {
        match self.applies.as_deref().map(|a| a.replace('-', "_")).as_deref() {
            Some("start") => Applies::Start,
            Some("next_turn") => Applies::NextTurn,
            _ => Applies::Now,
        }
    }

    pub fn choice(&self, value: &str) -> Option<&ChoiceData> {
        self.choice.iter().find(|c| c.value() == value)
    }

    /// The choice whose tags include `tag`.
    pub fn tagged(&self, tag: &str) -> Option<&ChoiceData> {
        self.choice.iter().find(|c| c.tags().contains(&tag))
    }

    /// The choices allowed while `other` (the option this depends on) has `value`.
    pub fn choices_for(&self, value: Option<&str>) -> Option<&WhenChoices> {
        let d = self.depends_on.first()?;
        d.when
            .iter()
            .find(|w| Some(w.name.as_str()) == value)
            .or_else(|| d.when.iter().find(|w| w.name == "*"))
    }

    /// This option as `ConfigOption`, with `value` and, when the harness reported them, its
    /// own `choices` in place of the data's.
    pub fn to_option(&self, value: Option<Value>, choices: Option<&[ChoiceData]>) -> ConfigOption {
        let schema = match choices {
            Some(c) => choices_schema(c, self.ty.as_deref()),
            // Choices the harness reports at runtime may be anything; the data's are examples.
            None if self.dynamic == Some(true) => {
                let examples: Vec<&str> = self.choice.iter().map(ChoiceData::value).collect();
                let mut s = json!({"type": self.ty.as_deref().unwrap_or("string")});
                if !examples.is_empty() {
                    s["examples"] = json!(examples);
                }
                s
            }
            None => choices_schema(&self.choice, self.ty.as_deref()),
        };
        let depends_on = self.depends_on.first().map(|d| DependsOn {
            option: d.name.clone(),
            schemas: d
                .when
                .iter()
                .map(|w| (w.name.clone(), choices_schema(&w.choice, self.ty.as_deref())))
                .collect(),
        });
        ConfigOption {
            id: self.name.clone(),
            role: self.role.as_deref().map(role),
            label: self.label.clone(),
            description: self.description.clone(),
            schema,
            value: value.unwrap_or(Value::Null),
            default: self.default.clone(),
            applies: self.applies(),
            depends_on,
            complete: None,
            meta: None,
            extra: Map::new(),
        }
    }
}

fn role(r: &str) -> Open<OptionRole> {
    serde_json::from_value(json!(r)).unwrap_or_else(|_| Open::Unknown(json!(r)))
}

// ---- levels.kdl ----

/// `levels.kdl`: the tools each level allows, and the level each combination of option values
/// gives (spec 15.2, Appendix E4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LevelsFile {
    /// `level "read" { tool "Read"; tool "mcp__*"; }`. A tool in no level is `host`.
    #[serde(default)]
    pub level: Vec<LevelTools>,
    /// `map level="write" network=#false { when permissionMode="acceptEdits"; }`, first match
    /// wins. Several `when` nodes are alternatives; the properties of one must all hold. A
    /// combination no `map` matches is `host` with network.
    #[serde(default)]
    pub map: Vec<LevelMap>,
    /// Arguments that pin the harness's settings sources under a ceiling.
    #[serde(default)]
    pub pin: Vec<PinArgs>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LevelTools {
    pub name: PermissionLevel,
    #[serde(default)]
    pub tool: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LevelMap {
    pub level: PermissionLevel,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub when: Vec<Map<String, Value>>,
}

/// `pin { arg "--setting-sources" "" }`: the arguments to pass, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PinArgs {
    #[serde(default)]
    pub arg: Vec<Value>,
}

impl LevelsFile {
    /// The lowest level whose tools include `tool`; `host` when none does.
    pub fn tool_level(&self, tool: &str) -> PermissionLevel {
        let mut levels: Vec<&LevelTools> = self.level.iter().collect();
        levels.sort_by_key(|l| l.name);
        levels
            .into_iter()
            .find(|l| l.tool.iter().any(|t| glob(t, tool)))
            .map_or(PermissionLevel::Host, |l| l.name)
    }

    /// The level and network flag of the option values in `values`.
    pub fn level_of(&self, values: &BTreeMap<String, Value>) -> (PermissionLevel, bool) {
        let holds = |w: &Map<String, Value>| w.iter().all(|(k, v)| values.get(k) == Some(v));
        self.map
            .iter()
            .find(|m| m.when.is_empty() || m.when.iter().any(holds))
            .map_or((PermissionLevel::Host, true), |m| (m.level, m.network))
    }

    /// The arguments that pin settings sources.
    pub fn pin_args(&self) -> Vec<String> {
        self.pin
            .iter()
            .flat_map(|p| p.arg.iter())
            .flat_map(|a| match a {
                Value::Array(xs) => xs.iter().map(value_str).collect::<Vec<_>>(),
                other => vec![value_str(other)],
            })
            .collect()
    }
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn glob(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

// ---- correlate.kdl ----

/// `correlate.kdl`: per item kind, how items of two sources match and which source wins
/// (spec 32.1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CorrelateFile {
    #[serde(default)]
    pub kind: Vec<CorrelateKind>,
}

/// `kind "tool" { match "ref" "position"; prefer "native"; history "transcript"; }`. `kind "*"`
/// covers the kinds not named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CorrelateKind {
    pub name: String,
    /// The keys tried in order: `ref` (the harness id), `position` (turn and order), `text`.
    #[serde(rename = "match", default)]
    pub matches: Vec<Value>,
    /// The source that wins while the binding runs.
    #[serde(default)]
    pub prefer: Vec<String>,
    /// The source that wins when history is read again.
    #[serde(default)]
    pub history: Vec<String>,
    /// Fields compared in shadow mode; all of `set` when empty.
    #[serde(default)]
    pub compare: Vec<Value>,
}

impl CorrelateFile {
    pub fn kind(&self, kind: &str) -> Option<&CorrelateKind> {
        self.kind
            .iter()
            .find(|k| k.name == kind)
            .or_else(|| self.kind.iter().find(|k| k.name == "*"))
    }

    /// The source that wins for `kind`: `live` while the binding runs, else in history.
    pub fn winner(&self, kind: &str, live: bool) -> &str {
        let k = self.kind(kind);
        fn pick(v: Option<&Vec<String>>) -> Option<&str> {
            v.and_then(|v| v.first()).map(String::as_str)
        }
        if live {
            pick(k.map(|k| &k.prefer)).unwrap_or("native")
        } else {
            pick(k.map(|k| &k.history)).unwrap_or("transcript")
        }
    }
}

impl CorrelateKind {
    pub fn keys(&self) -> Vec<String> {
        flatten_strings(&self.matches)
    }

    pub fn fields(&self) -> Vec<String> {
        flatten_strings(&self.compare)
    }
}

fn flatten_strings(v: &[Value]) -> Vec<String> {
    v.iter()
        .flat_map(|x| match x {
            Value::Array(xs) => xs.iter().map(value_str).collect::<Vec<_>>(),
            other => vec![value_str(other)],
        })
        .collect()
}

// ---- frames ----

/// Removes fields that carry secrets from raw source output before it goes into a frame
/// (spec 35): tokens, keys, passwords and authorization headers, at any depth.
pub fn scrub(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|k, _| !secret_key(k));
            m.values_mut().for_each(scrub);
        }
        Value::Array(xs) => xs.iter_mut().for_each(scrub),
        _ => {}
    }
}

fn secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase().replace('-', "_");
    // Usage counts end in `tokens`; they are not secrets.
    if k.ends_with("tokens") {
        return false;
    }
    k == "token"
        || k.ends_with("_token")
        || k.ends_with("token") && k.len() > 5 && !k.ends_with("_count")
        || [
            "api_key",
            "apikey",
            "authorization",
            "password",
            "secret",
            "client_secret",
            "cookie",
            "bearer",
        ]
        .iter()
        .any(|s| k == *s || k.ends_with(&format!("_{s}")))
}

/// A frame update's `data`: the raw `record` with the data `version` and the time it was read.
pub fn frame_data(version: &str, at: &str, mut record: Value) -> Value {
    scrub(&mut record);
    json!({"v": version, "at": at, "record": record})
}

/// The schemas of the data files, by file name, for `spec/schema/driver-data/`.
pub fn schemas() -> Vec<(&'static str, Value)> {
    vec![
        ("options", crate::schema::schema_for::<OptionsFile>()),
        ("levels", crate::schema::schema_for::<LevelsFile>()),
        ("correlate", crate::schema::schema_for::<CorrelateFile>()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const README_OPTIONS: &str = r#"
option "model" role="model" label="Model" applies="now" default="default" {
  choice "default" label="Default"
}
option "mode" role="mode" label="Mode" applies="next-turn" {
  choice "plan" label="Plan" tags="plan"
  choice "auto" label="Auto" tags="bypass"
}
option "effort" role="effort" label="Effort" {
  depends-on "model" {
    when "fast-model" { choice "low"; choice "high"; }
  }
}
"#;

    #[test]
    fn reads_the_frozen_options_shape() {
        let f: OptionsFile = parse("options.kdl", README_OPTIONS).unwrap();
        assert_eq!(f.option.len(), 3);
        let mode = f.get("mode").unwrap();
        assert_eq!(mode.applies(), Applies::NextTurn);
        assert_eq!(mode.tagged("bypass").unwrap().value(), "auto");
        let effort = f.get("effort").unwrap();
        let w = effort.choices_for(Some("fast-model")).unwrap();
        assert_eq!(
            w.choice.iter().map(ChoiceData::value).collect::<Vec<_>>(),
            ["low", "high"]
        );
        let o = f.get("model").unwrap().to_option(Some(json!("default")), None);
        assert_eq!(o.schema["oneOf"][0]["const"], "default");
        assert_eq!(o.default, Some(json!("default")));
        let e = effort.to_option(None, None);
        assert_eq!(
            e.depends_on.unwrap().schemas["fast-model"]["oneOf"][1]["const"],
            "high"
        );
    }

    #[test]
    fn reads_the_frozen_levels_and_correlate_shapes() {
        let l: LevelsFile = parse(
            "levels.kdl",
            r#"level "read" { tool "Read"; tool "Grep"; }
level "write" { tool "Edit"; tool "Write"; }
level "host" { tool "Bash"; }
map level="write" { when mode="edit"; }
map level="read" { when mode="plan"; when mode="default"; }"#,
        )
        .unwrap();
        assert_eq!(l.tool_level("Grep"), PermissionLevel::Read);
        assert_eq!(l.tool_level("Edit"), PermissionLevel::Write);
        assert_eq!(l.tool_level("WebFetch"), PermissionLevel::Host);
        let values = |m: &str| BTreeMap::from([("mode".to_owned(), json!(m))]);
        assert_eq!(l.level_of(&values("plan")), (PermissionLevel::Read, false));
        assert_eq!(l.level_of(&values("new")), (PermissionLevel::Host, true));
        let c: CorrelateFile = parse(
            "correlate.kdl",
            "kind \"tool\" {\n  match \"ref\"\n  prefer \"native\"\n}",
        )
        .unwrap();
        assert_eq!(c.winner("tool", true), "native");
        assert_eq!(c.winner("tool", false), "transcript");
        assert_eq!(c.kind("tool").unwrap().keys(), ["ref"]);
    }

    #[test]
    fn scrub_drops_secrets_and_keeps_usage() {
        let mut v = json!({"usage": {"input_tokens": 3}, "headers": {"Authorization": "x"}, "api_key": "k",
            "auth": {"accessToken": "t", "refresh_token": "r"}, "text": "hi"});
        scrub(&mut v);
        assert_eq!(
            v,
            json!({"usage": {"input_tokens": 3}, "headers": {}, "auth": {}, "text": "hi"})
        );
    }

    #[test]
    fn grouped_children() {
        let v = to_json(
            "a x=1 { b \"y\"; c 1 2; (array)d \"z\"; e; e; (array)f }",
            Children::Grouped,
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"a": {"x": 1, "b": "y", "c": [1, 2], "d": ["z"], "e": [{}, {}], "f": []}})
        );
    }

    #[test]
    fn committed_schemas_are_current() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/schema/driver-data");
        for (name, schema) in schemas() {
            let path = dir.join(format!("{name}.schema.json"));
            let fresh = serde_json::to_string_pretty(&schema).unwrap() + "\n";
            if std::env::var_os("PARROT_BLESS").is_some() {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&path, &fresh).unwrap();
                continue;
            }
            let committed = std::fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                committed,
                fresh,
                "{} is stale; run with PARROT_BLESS=1",
                path.display()
            );
        }
    }

    #[test]
    fn examples_parse() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/examples/driver-data");
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        parse::<OptionsFile>("options.kdl", &read("options.kdl")).unwrap();
        parse::<LevelsFile>("levels.kdl", &read("levels.kdl")).unwrap();
        parse::<CorrelateFile>("correlate.kdl", &read("correlate.kdl")).unwrap();
    }
}
