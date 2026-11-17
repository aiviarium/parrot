//! Profiles: declarative rules that tag and correct the raw tree for one program, name its
//! states, and define its actions. Read from TOML and evaluated here, without code of the host's own.
//!
//! - [`Profile::parse`] reads a profile and checks every rule, including the actions with
//!   their variables filled in.
//! - [`Profile::resolve`] applies the `[[variant]]` sections that match a CLI version.
//! - [`Profile::label`] applies the `[[node]]` rules and the [`Tagger`] hooks to a raw tree.
//! - [`Profile::evaluate`] finds which `[[state]]` rules hold; the first that holds is the
//!   current state.
//! - [`Profile::compile_action`] substitutes variables into an action and reads its steps.

mod types;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use types::*;

use crate::tree::{Node, Rect, Role, Tree};
use parrot_term::screen::{Attrs, Screen, normalize_spaces};

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("{path}: {message}")]
    Io { path: String, message: String },
    #[error("profile: {0}")]
    Parse(String),
    #[error("profile: action {action}: {message}")]
    Action { action: String, message: String },
}

/// The escape hatch: a function in the host program that receives the tree and returns
/// tags, as `(node id, tag)`. Named by `[[hook]]` in a profile. It cannot be tested by
/// replaying a recording alone, so it is a last resort.
pub trait Tagger: Send + Sync {
    fn tag(&self, tree: &Tree, screen: &Screen) -> Vec<(String, String)>;
}

impl<F> Tagger for F
where
    F: Fn(&Tree, &Screen) -> Vec<(String, String)> + Send + Sync,
{
    fn tag(&self, tree: &Tree, screen: &Screen) -> Vec<(String, String)> {
        self(tree, screen)
    }
}

/// Hooks by name.
pub type Hooks = HashMap<String, Arc<dyn Tagger>>;

/// A parsed profile.
#[derive(Clone, Debug)]
pub struct Profile {
    pub file: ProfileFile,
    pub path: Option<PathBuf>,
    /// The version the variants were resolved for.
    pub resolved_for: Option<String>,
}

/// Which states hold for a tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateEval {
    /// Every state whose rule holds, in profile order.
    pub holding: Vec<String>,
    /// The first of them.
    pub current: Option<String>,
}

/// An action with its variables substituted.
#[derive(Clone, Debug)]
pub struct CompiledAction {
    pub name: String,
    pub require: Option<Cond>,
    pub steps: Vec<Step>,
    pub expect: Option<Cond>,
}

/// What a condition is evaluated against.
pub struct Ctx<'a> {
    pub tree: &'a Tree,
    /// The screen's rows joined with `\n`.
    pub text: &'a str,
    /// States that have been current in this session.
    pub seen: &'a HashSet<String>,
    /// In an action: the text of each tag's first node when the action started.
    pub before: Option<&'a HashMap<String, String>>,
}

/// The text of each tag's first node, for `changed` conditions.
pub fn tag_texts(tree: &Tree) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for n in tree.nodes() {
        for t in &n.tags {
            m.entry(t.clone()).or_insert_with(|| n.text.clone());
        }
    }
    m
}

pub const DEFAULT_UNRECOGNISED_AFTER: Duration = Duration::from_secs(10);

impl Profile {
    pub fn parse(text: &str) -> Result<Profile, ProfileError> {
        let file: ProfileFile = toml::from_str(text).map_err(|e| ProfileError::Parse(e.to_string()))?;
        let p = Profile {
            file,
            path: None,
            resolved_for: None,
        };
        p.check()?;
        Ok(p)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Profile, ProfileError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| ProfileError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        let mut p = Profile::parse(&text).map_err(|e| ProfileError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        p.path = Some(path.to_path_buf());
        Ok(p)
    }

    pub fn id(&self) -> &str {
        &self.file.profile.id
    }

    pub fn unrecognised_after(&self) -> Duration {
        self.file
            .profile
            .unrecognised_after
            .map_or(DEFAULT_UNRECOGNISED_AFTER, |d| d.0)
    }

    /// Checks that every action reads with its variables set to a placeholder, and that
    /// variants have valid version ranges.
    fn check(&self) -> Result<(), ProfileError> {
        let mut all: Vec<&ActionDef> = self.file.action.iter().collect();
        for v in &self.file.variant {
            if parse_range(&v.version).is_none() {
                return Err(ProfileError::Parse(format!(
                    "variant: bad version range {:?}",
                    v.version
                )));
            }
            all.extend(&v.action);
        }
        for a in all {
            let vars = Vars::placeholder();
            compile(a, &vars).map_err(|message| ProfileError::Action {
                action: a.name.clone(),
                message,
            })?;
        }
        Ok(())
    }

    /// The profile with the variants for `version` applied.
    pub fn resolve(&self, version: Option<&str>) -> Profile {
        let mut file = self.file.clone();
        let variants = std::mem::take(&mut file.variant);
        if let Some(version) = version {
            for v in variants.iter().filter(|v| version_in_range(version, &v.version)) {
                for r in &v.node {
                    match file.node.iter_mut().find(|x| x.tag == r.tag) {
                        Some(x) => *x = r.clone(),
                        None => file.node.push(r.clone()),
                    }
                }
                for r in &v.state {
                    match file.state.iter_mut().find(|x| x.name == r.name) {
                        Some(x) => *x = r.clone(),
                        None => file.state.push(r.clone()),
                    }
                }
                for r in &v.action {
                    match file.action.iter_mut().find(|x| x.name == r.name) {
                        Some(x) => *x = r.clone(),
                        None => file.action.push(r.clone()),
                    }
                }
            }
        }
        file.variant = variants;
        Profile {
            file,
            path: self.path.clone(),
            resolved_for: version.map(str::to_string),
        }
    }

    /// Reads the CLI's version from the screen, if the profile says how.
    pub fn version_from_screen(&self, screen: &Screen) -> Option<String> {
        let re = self.file.profile.version_from_screen.as_ref()?;
        let text = screen.lines().join("\n");
        re.0.captures(&text).map(|c| c[1].to_string())
    }

    pub fn action(&self, name: &str) -> Option<&ActionDef> {
        self.file.action.iter().find(|a| a.name == name)
    }

    pub fn compile_action(
        &self,
        name: &str,
        vars: &HashMap<String, String>,
    ) -> Result<CompiledAction, String> {
        let a = self.action(name).ok_or_else(|| format!("no action {name:?}"))?;
        compile(a, &Vars::Map(vars))
    }

    /// Applies the node rules, then the hooks.
    pub fn label(&self, tree: &mut Tree, screen: &Screen, hooks: &Hooks) {
        for rule in &self.file.node {
            apply_rule(rule, tree, screen);
        }
        for h in &self.file.hook {
            if let Some(t) = hooks.get(&h.name) {
                let tags = t.tag(tree, screen);
                for (id, tag) in tags {
                    let src = format!("+hook:{}", h.name);
                    tree.root.walk_mut(&mut |n| {
                        if n.id == id {
                            n.add_tag(&tag);
                            if !n.source.contains(&src) {
                                n.source.push_str(&src);
                            }
                        }
                    });
                }
            }
        }
    }

    /// Which states hold. A state rule can refer to other states by name; a state with
    /// several rules holds when any of them does. The current state is the name of the first
    /// rule, in file order, that holds.
    pub fn evaluate(&self, ctx: &Ctx) -> StateEval {
        let memo: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
        let busy: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
        fn state_holds(
            p: &Profile,
            name: &str,
            ctx: &Ctx,
            memo: &RefCell<HashMap<String, bool>>,
            busy: &RefCell<HashSet<String>>,
        ) -> bool {
            if let Some(&v) = memo.borrow().get(name) {
                return v;
            }
            if !busy.borrow_mut().insert(name.to_string()) {
                return false;
            }
            let v = p
                .file
                .state
                .iter()
                .filter(|s| s.name == name)
                .any(|rule| eval_cond(&rule.when, ctx, &|n| state_holds(p, n, ctx, memo, busy)).is_ok());
            busy.borrow_mut().remove(name);
            memo.borrow_mut().insert(name.to_string(), v);
            v
        }
        let mut holding: Vec<String> = Vec::new();
        let mut current = None;
        for rule in &self.file.state {
            if !state_holds(self, &rule.name, ctx, &memo, &busy) {
                continue;
            }
            if current.is_none()
                && eval_cond(&rule.when, ctx, &|n| state_holds(self, n, ctx, &memo, &busy)).is_ok()
            {
                current = Some(rule.name.clone());
            }
            if !holding.contains(&rule.name) {
                holding.push(rule.name.clone());
            }
        }
        StateEval { holding, current }
    }

    /// State names in file order, each once.
    pub fn state_names(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for s in &self.file.state {
            if !v.contains(&s.name) {
                v.push(s.name.clone());
            }
        }
        v
    }
}

/// Evaluates a condition where `state = X` means the current state is X.
pub fn eval_wait(cond: &Cond, ctx: &Ctx, current: Option<&str>) -> Result<(), String> {
    eval_cond(cond, ctx, &|n| current == Some(n))
}

enum Vars<'a> {
    Map(&'a HashMap<String, String>),
    Placeholder,
}

impl Vars<'_> {
    fn placeholder() -> Vars<'static> {
        Vars::Placeholder
    }
    fn get(&self, name: &str) -> Option<String> {
        match self {
            Vars::Map(m) => m.get(name).cloned(),
            Vars::Placeholder => Some("0".into()),
        }
    }
}

fn substitute_str(s: &str, vars: &Vars) -> Result<String, String> {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '$' {
            out.push(c);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'$') {
            out.push('$');
            i += 2;
            continue;
        }
        let (name, next) = if chars.get(i + 1) == Some(&'{') {
            let end = chars[i + 2..]
                .iter()
                .position(|&c| c == '}')
                .ok_or_else(|| format!("unclosed ${{ in {s:?}"))?;
            (chars[i + 2..i + 2 + end].iter().collect::<String>(), i + 3 + end)
        } else {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '-')
            {
                j += 1;
            }
            (chars[i + 1..j].iter().collect::<String>(), j)
        };
        if name.is_empty() {
            out.push('$');
            i += 1;
            continue;
        }
        let v = vars
            .get(&name)
            .ok_or_else(|| format!("missing variable ${name}"))?;
        out.push_str(&v);
        i = next;
    }
    Ok(out)
}

fn substitute(v: &toml::Value, vars: &Vars) -> Result<toml::Value, String> {
    Ok(match v {
        toml::Value::String(s) => toml::Value::String(substitute_str(s, vars)?),
        toml::Value::Array(a) => {
            toml::Value::Array(a.iter().map(|x| substitute(x, vars)).collect::<Result<_, _>>()?)
        }
        toml::Value::Table(t) => {
            let mut out = toml::map::Map::new();
            for (k, x) in t {
                out.insert(k.clone(), substitute(x, vars)?);
            }
            toml::Value::Table(out)
        }
        other => other.clone(),
    })
}

fn read_cond(v: &toml::Value) -> Result<Cond, String> {
    v.clone().try_into::<Cond>().map_err(|e| e.to_string())
}

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(5);

fn read_step(v: &toml::Value) -> Result<Step, String> {
    let t = v.as_table().ok_or("a step must be a table")?;
    let strings = |x: &toml::Value| -> Result<Vec<String>, String> {
        match x {
            toml::Value::String(s) => Ok(vec![s.clone()]),
            toml::Value::Array(a) => a
                .iter()
                .map(|e| {
                    e.as_str()
                        .map(str::to_string)
                        .ok_or("keys must be strings".into())
                })
                .collect(),
            _ => Err("keys must be strings".into()),
        }
    };
    if let Some(k) = t.get("key").or_else(|| t.get("keys")) {
        let keys = strings(k)?;
        for key in &keys {
            if parrot_term::input::parse_key(key).is_none() {
                return Err(format!("unknown key {key:?}"));
            }
        }
        return Ok(Step::Key(keys));
    }
    if let Some(s) = t.get("type") {
        return Ok(Step::Type(s.as_str().ok_or("type takes a string")?.into()));
    }
    if let Some(s) = t.get("paste") {
        return Ok(Step::Paste(s.as_str().ok_or("paste takes a string")?.into()));
    }
    if let Some(s) = t.get("select") {
        let sel: SelectStep = s.clone().try_into().map_err(|e| e.to_string())?;
        if sel.index.is_none() && sel.label.is_none() && sel.option.is_none() {
            return Err("select needs index, label or option".into());
        }
        return Ok(Step::Select(sel));
    }
    if let Some(w) = t.get("wait") {
        let cond = read_cond(w)?;
        let timeout = match t.get("timeout") {
            Some(x) => x.clone().try_into::<Dur>().map_err(|e| e.to_string())?.0,
            None => cond.within.map_or(DEFAULT_STEP_TIMEOUT, |d| d.0),
        };
        return Ok(Step::Wait {
            cond: Box::new(cond),
            timeout,
        });
    }
    Err(format!(
        "unknown step {:?}",
        t.keys().cloned().collect::<Vec<_>>()
    ))
}

fn compile(a: &ActionDef, vars: &Vars) -> Result<CompiledAction, String> {
    let require = match &a.require {
        Some(v) => Some(read_cond(&substitute(v, vars)?).map_err(|e| format!("require: {e}"))?),
        None => None,
    };
    let expect = match &a.expect {
        Some(v) => Some(read_cond(&substitute(v, vars)?).map_err(|e| format!("expect: {e}"))?),
        None => None,
    };
    let steps = a
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            substitute(s, vars)
                .and_then(|s| read_step(&s))
                .map_err(|e| format!("step {}: {e}", i + 1))
        })
        .collect::<Result<_, _>>()?;
    Ok(CompiledAction {
        name: a.name.clone(),
        require,
        steps,
        expect,
    })
}

// ---- versions ----

fn parse_version(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .take_while(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

fn cmp_version(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => {}
            o => return o,
        }
    }
    std::cmp::Ordering::Equal
}

type Range = Vec<(String, Vec<u64>)>;

fn parse_range(r: &str) -> Option<Range> {
    let mut out = Vec::new();
    for part in r.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let op_len = part
            .find(|c: char| c.is_ascii_digit() || c == 'v')
            .unwrap_or(part.len());
        let (op, v) = part.split_at(op_len);
        let op = op.trim();
        if !matches!(op, "" | "=" | ">=" | "<=" | ">" | "<") {
            return None;
        }
        let v = parse_version(v);
        if v.is_empty() {
            return None;
        }
        out.push((op.to_string(), v));
    }
    (!out.is_empty()).then_some(out)
}

/// Whether `version` is in a range such as `>=2.1, <2.2`.
pub fn version_in_range(version: &str, range: &str) -> bool {
    let Some(range) = parse_range(range) else {
        return false;
    };
    let v = parse_version(version);
    range.iter().all(|(op, b)| {
        let o = cmp_version(&v, b);
        use std::cmp::Ordering::*;
        match op.as_str() {
            "" | "=" => {
                // `=2.1` matches 2.1.x.
                v.len() >= b.len() && cmp_version(&v[..b.len()], b) == Equal
            }
            ">=" => o != Less,
            "<=" => o != Greater,
            ">" => o == Greater,
            "<" => o == Less,
            _ => false,
        }
    })
}

// ---- matching ----

fn squash(s: &str) -> String {
    normalize_spaces(s)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// The text a condition compares: an input's value if it has one, otherwise its text.
fn node_value(n: &Node) -> &str {
    n.value.as_deref().unwrap_or(&n.text)
}

pub fn node_matches(m: &Matcher, n: &Node, tree: &Tree) -> bool {
    if let Some(roles) = &m.role
        && !roles.as_slice().contains(&n.role)
    {
        return false;
    }
    if let Some(t) = &m.tag
        && !n.has_tag(t)
    {
        return false;
    }
    if let Some(s) = &m.source
        && !n.source.starts_with(s.as_str())
    {
        return false;
    }
    if let Some(b) = m.is_box
        && n.source.starts_with("auto:box") != b
    {
        return false;
    }
    if let Some(want) = m.contains_cursor {
        let has = tree.cursor.is_some_and(|(r, c)| {
            n.bounds.contains(r, c) || (n.role == Role::Input && n.bounds.row <= r && r < n.bounds.bottom())
        });
        if has != want {
            return false;
        }
    }
    for (want, have) in [
        (m.focused, n.state.focused),
        (m.selected, n.state.selected),
        (m.busy, n.state.busy),
        (m.bold, n.style.bold),
        (m.dim, n.style.dim),
        (m.italic, n.style.italic),
        (m.reverse, n.style.reverse),
    ] {
        if want.is_some_and(|w| w != have) {
            return false;
        }
    }
    if let Some(p) = m.position {
        let rows = tree.size.rows as u32;
        let ok = match p {
            Position::Top => (n.bounds.row as u32) * 3 < rows,
            Position::Bottom => (n.bounds.bottom() as u32) * 3 > rows * 2,
        };
        if !ok {
            return false;
        }
    }
    if let Some(re) = &m.text_matches
        && !re.0.is_match(&n.text)
    {
        return false;
    }
    if let Some(s) = &m.text_contains
        && !squash(&n.text).contains(&squash(s))
    {
        return false;
    }
    if let Some(s) = &m.starts_with
        && !n.text.trim_start().starts_with(s.as_str())
    {
        return false;
    }
    if let Some(re) = &m.first_line_matches
        && !re.0.is_match(n.first_line())
    {
        return false;
    }
    if let Some(re) = &m.label_matches
        && !n.label.as_deref().is_some_and(|l| re.0.is_match(l))
    {
        return false;
    }
    if let Some(re) = &m.value_matches
        && !n.value.as_deref().is_some_and(|v| re.0.is_match(v))
    {
        return false;
    }
    if let Some(fg) = &m.fg
        && n.style.fg.as_deref() != Some(fg.as_str())
    {
        return false;
    }
    if let Some(bg) = &m.bg
        && n.style.bg.as_deref() != Some(bg.as_str())
    {
        return false;
    }
    if m.min_rows.is_some_and(|r| n.bounds.rows < r) || m.max_rows.is_some_and(|r| n.bounds.rows > r) {
        return false;
    }
    if let Some(tag) = &m.below {
        match tree.find_tag(tag) {
            Some(t) if n.bounds.row >= t.bounds.bottom() => {}
            _ => return false,
        }
    }
    if let Some(tag) = &m.above {
        match tree.find_tag(tag) {
            Some(t) if n.bounds.bottom() <= t.bounds.row => {}
            _ => return false,
        }
    }
    if let Some(h) = &m.has
        && !n.walk().into_iter().skip(1).any(|d| node_matches(h, d, tree))
    {
        return false;
    }
    if let Some(x) = &m.not
        && node_matches(x, n, tree)
    {
        return false;
    }
    if let Some(any) = &m.any
        && !any.iter().any(|x| node_matches(x, n, tree))
    {
        return false;
    }
    true
}

fn line_matches(m: &LineMatcher, screen: &Screen, row: u16, rect: Rect, text: &str) -> bool {
    if let Some(s) = &m.starts_with
        && !text.trim_start().starts_with(s.as_str())
    {
        return false;
    }
    if let Some(re) = &m.matches
        && !re.0.is_match(text)
    {
        return false;
    }
    let styles: Vec<parrot_term::screen::Style> = screen
        .row(row)
        .map(|r| {
            (rect.col..rect.right())
                .map(|c| r.cell(c))
                .filter(|c| !c.is_blank())
                .map(|c| c.style())
                .collect()
        })
        .unwrap_or_default();
    let most = |f: &dyn Fn(&parrot_term::screen::Style) -> bool| {
        !styles.is_empty() && styles.iter().filter(|s| f(s)).count() * 2 > styles.len()
    };
    if let Some(r) = m.reverse
        && most(&|s| s.attrs.contains(Attrs::REVERSE)) != r
    {
        return false;
    }
    if let Some(b) = m.bold
        && most(&|s| s.attrs.contains(Attrs::BOLD)) != b
    {
        return false;
    }
    if let Some(fg) = &m.fg
        && !most(&|s| crate::tree::color_name(s.fg).as_deref() == Some(fg.as_str()))
    {
        return false;
    }
    true
}

fn row_text(screen: &Screen, row: u16, rect: Rect) -> String {
    let t = screen
        .row(row)
        .map(|r| r.text_range(rect.col, rect.right()))
        .unwrap_or_default();
    normalize_spaces(t.trim_end()).into_owned()
}

fn region_text(screen: &Screen, rect: Rect) -> String {
    let mut lines: Vec<String> = (rect.row..rect.bottom())
        .map(|r| row_text(screen, r, rect))
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn apply_rule(rule: &NodeRule, tree: &mut Tree, screen: &Screen) {
    let name = rule.name.clone().unwrap_or_else(|| rule.tag.clone());
    let scopes: Vec<(String, Rect)> = match &rule.inside {
        Some(tag) => tree
            .tagged(tag)
            .iter()
            .map(|n| (n.id.clone(), n.bounds))
            .collect(),
        None => vec![(tree.root.id.clone(), tree.root.bounds)],
    };
    if let Some(lines) = &rule.lines {
        for (_, scope) in &scopes {
            let rows: Vec<u16> = (scope.row..scope.bottom()).collect();
            let from = if lines.last {
                rows.iter()
                    .rev()
                    .find(|&&r| lines.from.0.is_match(&row_text(screen, r, *scope)))
            } else {
                rows.iter()
                    .find(|&&r| lines.from.0.is_match(&row_text(screen, r, *scope)))
            };
            let Some(&r0) = from else { continue };
            let r1 = match (&lines.to, &lines.through) {
                (Some(to), _) => rows
                    .iter()
                    .copied()
                    .find(|&r| r >= r0 && to.0.is_match(&row_text(screen, r, *scope))),
                (None, Some(th)) => rows
                    .iter()
                    .rev()
                    .copied()
                    .find(|&r| r >= r0 && th.0.is_match(&row_text(screen, r, *scope))),
                (None, None) => rows
                    .iter()
                    .rev()
                    .copied()
                    .find(|&r| r >= r0 && !row_text(screen, r, *scope).trim().is_empty()),
            };
            let Some(r1) = r1 else { continue };
            let rect = Rect::new(r0, scope.col, r1 - r0 + 1, scope.cols);
            let mut node = Node::new(
                rule.role.unwrap_or(Role::Pane),
                rect,
                region_text(screen, rect),
                &format!("profile:{name}"),
            );
            node.id = format!("p-{}-{}", rule.tag, r0);
            node.add_tag(&rule.tag);
            if let Some(re) = &rule.label {
                node.label = re.0.captures(&node.text).map(|c| c[1].trim().to_string());
            }
            if let Some(m) = &rule.matcher
                && !node_matches(m, &node, tree)
            {
                continue;
            }
            let id = node.id.clone();
            insert_group(&mut tree.root, node);
            // A group's rows read as items (a question's options, split by a rule).
            if let Some(spec) = &rule.item {
                tree.root.walk_mut(&mut |n| {
                    if n.id == id {
                        split_items(n, spec, screen, &name);
                    }
                });
            }
            if rule.first {
                break;
            }
        }
        return;
    }
    let Some(m) = &rule.matcher else { return };
    let mut ids: Vec<String> = Vec::new();
    for (scope_id, _) in &scopes {
        let Some(scope) = tree.by_id(scope_id) else {
            continue;
        };
        for n in scope.walk().into_iter().skip(1) {
            if node_matches(m, n, tree) && !ids.contains(&n.id) {
                ids.push(n.id.clone());
            }
        }
    }
    if rule.first {
        ids.truncate(1);
    }
    if rule.last && ids.len() > 1 {
        ids.drain(..ids.len() - 1);
    }
    let src = format!("+profile:{name}");
    tree.root.walk_mut(&mut |n| {
        if !ids.contains(&n.id) {
            return;
        }
        n.add_tag(&rule.tag);
        if !n.source.contains(&src) {
            n.source.push_str(&src);
        }
        if let Some(role) = rule.role {
            n.role = role;
        }
        if let Some(re) = &rule.label
            && let Some(c) = re.0.captures(&n.text)
        {
            n.label = Some(c[1].trim().to_string());
        }
        if let Some(spec) = &rule.item {
            split_items(n, spec, screen, &name);
        }
    });
}

/// Puts `node` into the tree: into the deepest container holding its rows, adopting the
/// children whose rows it covers.
fn insert_group(root: &mut Node, mut node: Node) {
    let rect = node.bounds;
    let mut parent = root;
    loop {
        let idx = parent.children.iter().position(|c| {
            c.role.is_container()
                && c.bounds.row <= rect.row
                && c.bounds.bottom() >= rect.bottom()
                && !(c.bounds.row == rect.row && c.bounds.bottom() == rect.bottom())
        });
        match idx {
            Some(i) => parent = &mut parent.children[i],
            None => break,
        }
    }
    let inside = |c: &Node| c.bounds.row >= rect.row && c.bounds.bottom() <= rect.bottom();
    let first = parent.children.iter().position(&inside);
    let pos = first.unwrap_or_else(|| {
        parent
            .children
            .iter()
            .position(|c| c.bounds.row >= rect.row)
            .unwrap_or(parent.children.len())
    });
    let mut kept = Vec::new();
    for (i, c) in std::mem::take(&mut parent.children).into_iter().enumerate() {
        if inside(&c) {
            node.children.push(c);
        } else {
            kept.push((i, c));
        }
    }
    let mut out = Vec::new();
    let mut inserted = false;
    for (i, c) in kept {
        if !inserted && i >= pos {
            out.push(std::mem::replace(&mut node, Node::new(Role::Text, rect, "", "")));
            inserted = true;
        }
        out.push(c);
    }
    if !inserted {
        out.push(node);
    }
    parent.children = out;
}

static KEY_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^\s*\S?\s*(\d{1,2})[.)]\s").unwrap());

fn split_items(n: &mut Node, spec: &ItemSpec, screen: &Screen, rule: &str) {
    let rect = n.bounds;
    let item_role = match n.role {
        Role::List => Role::Listitem,
        _ => Role::Menuitem,
    };
    let mut items: Vec<Node> = Vec::new();
    let mut open = false;
    for r in rect.row..rect.bottom() {
        let text = row_text(screen, r, rect);
        if text.trim().is_empty() {
            open = false;
            continue;
        }
        let rule_only = text
            .trim()
            .chars()
            .all(|c| matches!(c, '─' | '━' | '═' | '╌' | '┄' | '┈' | '▔' | '▁' | '-' | ' '));
        if rule_only || spec.ignore.as_ref().is_some_and(|re| re.0.is_match(&text)) {
            continue;
        }
        if spec.line_matches.0.is_match(&text) {
            let mut it = Node::new(
                item_role,
                Rect::new(r, rect.col, 1, rect.cols),
                text.trim().to_string(),
                &format!("profile:{rule}"),
            );
            if let Some(sel) = &spec.selected_when {
                it.state.selected = line_matches(sel, screen, r, rect, &text);
            }
            it.key = match &spec.key {
                Some(re) => re.0.captures(&text).map(|c| c[1].to_string()),
                None => KEY_RE.captures(&text).map(|c| c[1].to_string()),
            };
            it.label = Some(match &spec.label {
                Some(re) => {
                    re.0.captures(&text)
                        .map(|c| c[1].trim().to_string())
                        .unwrap_or_else(|| text.trim().to_string())
                }
                None => {
                    let t = text.trim_start();
                    let t = t
                        .trim_start_matches(['❯', '›', '>', '▶', '►', '➤', '→'])
                        .trim_start();
                    match KEY_RE.captures(&text) {
                        Some(c) => t[t.find(&c[1]).map_or(0, |i| i + c[1].len() + 1)..]
                            .trim()
                            .to_string(),
                        None => t.trim().to_string(),
                    }
                }
            });
            items.push(it);
            open = true;
        } else if open && let Some(last) = items.last_mut() {
            last.bounds.rows += 1;
            last.text.push('\n');
            last.text.push_str(text.trim());
            if let Some(l) = &mut last.label {
                l.push(' ');
                l.push_str(text.trim());
            }
        }
    }
    for (i, it) in items.iter_mut().enumerate() {
        it.id = format!("{}-item{i}", n.id);
    }
    n.children = items;
}

// ---- conditions ----

fn tags(t: &OneOrMany<String>) -> &[String] {
    t.as_slice()
}

/// Evaluates a condition. `is_state(name)` answers `state = name`. Returns why it fails.
pub fn eval_cond(cond: &Cond, ctx: &Ctx, is_state: &dyn Fn(&str) -> bool) -> Result<(), String> {
    let tree = ctx.tree;
    if let Some(s) = &cond.state
        && !s.as_slice().iter().any(|n| is_state(n))
    {
        return Err(format!("state is not {}", s.as_slice().join(" or ")));
    }
    if let Some(t) = &cond.exists {
        for tag in tags(t) {
            if tree.find_tag(tag).is_none() {
                return Err(format!("no node tagged {tag}"));
            }
        }
    }
    if let Some(t) = &cond.not_exists {
        for tag in tags(t) {
            if tree.find_tag(tag).is_some() {
                return Err(format!("a node is tagged {tag}"));
            }
        }
    }
    if let Some(m) = &cond.find
        && !tree.nodes().into_iter().any(|n| node_matches(m, n, tree))
    {
        return Err("no node matches find".into());
    }
    if let Some(re) = &cond.any_text_matches
        && !re.0.is_match(ctx.text)
    {
        return Err(format!("screen does not match {:?}", re.0.as_str()));
    }
    if let Some(re) = &cond.no_text_matches
        && re.0.is_match(ctx.text)
    {
        return Err(format!("screen matches {:?}", re.0.as_str()));
    }
    if let Some(re) = &cond.title_matches
        && !tree.title.as_deref().is_some_and(|t| re.0.is_match(t))
    {
        return Err(format!("title does not match {:?}", re.0.as_str()));
    }
    if let Some(a) = cond.alt_screen
        && tree.alt_screen != a
    {
        return Err(format!("alt-screen is {}", tree.alt_screen));
    }
    if let Some(v) = cond.cursor_visible
        && tree.cursor.is_some() != v
    {
        return Err(format!("cursor visible is {}", tree.cursor.is_some()));
    }
    if let Some(tag) = &cond.focused {
        let ok = tree.tagged(tag).iter().any(|n| {
            n.state.focused
                || n.walk().iter().any(|d| d.state.focused)
                || tree.cursor.is_some_and(|(r, c)| n.bounds.contains(r, c))
        });
        if !ok {
            return Err(format!("{tag} is not focused"));
        }
    }
    if let Some(tag) = &cond.text_in {
        let Some(n) = tree.find_tag(tag) else {
            return Err(format!("no node tagged {tag}"));
        };
        let v = node_value(n);
        if let Some(c) = &cond.contains
            && !squash(v).contains(&squash(c))
        {
            return Err(format!("{tag} text {v:?} does not contain {c:?}"));
        }
        if let Some(re) = &cond.matches
            && !re.0.is_match(v)
        {
            return Err(format!("{tag} text {v:?} does not match {:?}", re.0.as_str()));
        }
    }
    if let Some(tag) = &cond.empty {
        let Some(n) = tree.find_tag(tag) else {
            return Err(format!("no node tagged {tag}"));
        };
        if !node_value(n).trim().is_empty() {
            return Err(format!("{tag} is not empty: {:?}", node_value(n)));
        }
    }
    if let Some(tag) = &cond.selected_in {
        let Some(n) = tree.find_tag(tag) else {
            return Err(format!("no node tagged {tag}"));
        };
        let items = n.items();
        let sel = n.selected_index();
        if let Some(i) = &cond.index {
            let want = i.get().ok_or("index is not a number")?;
            if sel.map(|s| s as i64) != Some(want) {
                return Err(format!("selected in {tag} is {sel:?}, not {want}"));
            }
        }
        if let Some(l) = &cond.label {
            let ok = sel.is_some_and(|s| items[s].display().to_lowercase().contains(&l.to_lowercase()));
            if !ok {
                return Err(format!("selected in {tag} is not {l:?}"));
            }
        }
    }
    if let Some(tag) = &cond.changed {
        let Some(before) = ctx.before else {
            return Err("changed only applies in an action".into());
        };
        let now = tree.find_tag(tag).map(|n| &n.text);
        if now.is_some() && now == before.get(tag) {
            return Err(format!("{tag} has not changed"));
        }
    }
    if let Some(s) = &cond.until_seen
        && ctx.seen.contains(s)
    {
        return Err(format!("{s} was seen"));
    }
    if let Some(s) = &cond.seen
        && !ctx.seen.contains(s)
    {
        return Err(format!("{s} not seen yet"));
    }
    match &cond.not {
        Some(NotCond::States(names)) => {
            if let Some(n) = names.iter().find(|n| is_state(n)) {
                return Err(format!("state {n} holds"));
            }
        }
        Some(NotCond::Cond(c)) if eval_cond(c, ctx, is_state).is_ok() => {
            return Err("not-condition holds".into());
        }
        Some(NotCond::Cond(_)) => {}
        None => {}
    }
    if let Some(all) = &cond.all {
        for c in all {
            eval_cond(c, ctx, is_state)?;
        }
    }
    if let Some(any) = &cond.any {
        let mut why = Vec::new();
        for c in any {
            match eval_cond(c, ctx, is_state) {
                Ok(()) => return Ok(()),
                Err(e) => why.push(e),
            }
        }
        return Err(format!("none of: {}", why.join("; ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parrot_term::screen::Size;

    const P: &str = r#"
[profile]
id = "test"
unrecognised-after = "3s"

[[node]]
tag = "prompt"
where = { role = "input", contains-cursor = true }

[[node]]
tag = "approval"
role = "dialog"
lines = { from = 'Do you want', to = 'Esc to' }

[[node]]
tag = "choices"
inside = "approval"
where = { role = "menu" }

[[state]]
name = "awaiting-approval"
when = { exists = "approval" }

[[state]]
name = "idle"
when = { exists = "prompt", not = ["awaiting-approval"] }

[[action]]
name = "submit"
require = { state = "idle" }
steps = [{ paste = "$text" }, { wait = { text-in = "prompt", contains = "$text" } }, { key = "enter" }]
expect = { state = "working", within = "5s" }

[[variant]]
version = ">=3"
[[variant.state]]
name = "idle"
when = { exists = "prompt" }
"#;

    fn tree_of(text: &str, rows: u16, cols: u16, cursor: Option<(u16, u16)>) -> (Tree, Screen) {
        let s = Screen::from_text(Size::new(rows, cols), text);
        let s = match cursor {
            Some((r, c)) => s.with(|cur, _, _, _| {
                cur.row = r;
                cur.col = c;
                cur.visible = true;
            }),
            None => s.with(|cur, _, _, _| cur.visible = false),
        };
        (crate::tree::segment(&s), s)
    }

    #[test]
    fn parses_labels_and_evaluates() {
        let p = Profile::parse(P).unwrap();
        assert_eq!(p.unrecognised_after(), Duration::from_secs(3));
        let rule = "─".repeat(30);
        let (mut t, s) = tree_of(&format!("hi\n{rule}\n❯ abc\n{rule}"), 4, 30, Some((2, 5)));
        p.label(&mut t, &s, &Hooks::new());
        assert_eq!(t.find_tag("prompt").unwrap().value.as_deref(), Some("abc"));
        let seen = HashSet::new();
        let text = s.lines().join("\n");
        let ctx = Ctx {
            tree: &t,
            text: &text,
            seen: &seen,
            before: None,
        };
        assert_eq!(p.evaluate(&ctx).current.as_deref(), Some("idle"));

        let (mut t, s) = tree_of(
            "log\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel",
            6,
            30,
            None,
        );
        p.label(&mut t, &s, &Hooks::new());
        let a = t.find_tag("approval").unwrap();
        assert_eq!(a.role, Role::Dialog);
        assert_eq!(a.bounds.row, 1);
        assert_eq!(t.find_tag("choices").unwrap().items().len(), 2);
        let text = s.lines().join("\n");
        let ctx = Ctx {
            tree: &t,
            text: &text,
            seen: &seen,
            before: None,
        };
        assert_eq!(p.evaluate(&ctx).current.as_deref(), Some("awaiting-approval"));
    }

    #[test]
    fn actions_substitute_variables() {
        let p = Profile::parse(P).unwrap();
        let vars = HashMap::from([("text".to_string(), "hello".to_string())]);
        let a = p.compile_action("submit", &vars).unwrap();
        assert!(matches!(&a.steps[0], Step::Paste(t) if t == "hello"));
        assert!(p.compile_action("submit", &HashMap::new()).is_err());
        let bad = P.replace(r#"{ key = "enter" }"#, r#"{ key = "nonsense-key" }"#);
        assert!(Profile::parse(&bad).is_err());
    }

    #[test]
    fn versions_and_variants() {
        assert!(version_in_range("2.1.325", ">=2.0"));
        assert!(version_in_range("2.1.325", ">=2.1, <2.2"));
        assert!(!version_in_range("2.2.0", ">=2.1, <2.2"));
        assert!(version_in_range("2.1.325", "=2.1"));
        let p = Profile::parse(P).unwrap();
        let r = p.resolve(Some("3.0.1"));
        let idle = r.file.state.iter().find(|s| s.name == "idle").unwrap();
        assert!(idle.when.not.is_none());
        let r = p.resolve(Some("2.0"));
        let idle = r.file.state.iter().find(|s| s.name == "idle").unwrap();
        assert!(idle.when.not.is_some());
    }

    #[test]
    fn hooks_add_tags() {
        let mut p = Profile::parse(P).unwrap();
        p.file.hook.push(HookRule {
            name: "mark-first".into(),
        });
        let mut hooks = Hooks::new();
        hooks.insert(
            "mark-first".into(),
            Arc::new(|t: &Tree, _: &Screen| vec![(t.root.children[0].id.clone(), "first".to_string())]),
        );
        let (mut t, s) = tree_of("hello\n\nworld", 3, 20, None);
        p.label(&mut t, &s, &hooks);
        assert!(t.root.children[0].has_tag("first"));
        assert!(t.root.children[0].source.contains("+hook:mark-first"));
    }
}
