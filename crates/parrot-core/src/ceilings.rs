//! Ceilings (spec 22, Appendix E4, E5): what a session and its subtree may do at most. A child's
//! ceiling is its parent's intersected with the one asked for, so ceilings only ever narrow.

use crate::{Cx, State};
use parrot_proto::option::PermissionLevel;
use parrot_proto::principal::{Isolation, Scope};
use parrot_proto::request::RequestStatus;
use parrot_proto::usage::{Budget, Cost};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The `Ceiling` of spec 22. Every field is a limit; an absent one doesn't limit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ceiling {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<PermissionLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<Isolation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub functions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unattended: Option<u64>,
}

/// Requests waiting for their `unattended` time to pass, by request id, to when it passes.
#[derive(Debug, Default)]
pub struct Ceilings {
    pub unattended: BTreeMap<String, u64>,
}

fn min_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn max_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Whether `name` matches a pattern with `*` wildcards.
pub fn glob(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else {
                return false;
            };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(at) = rest.find(part) {
            rest = &rest[at + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Whether `path` is `root` or inside it.
pub fn inside(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    path == root || path.strip_prefix(root).is_some_and(|r| r.starts_with('/')) || root.is_empty()
}

fn lists(
    parent: &Option<Vec<String>>,
    asked: &Option<Vec<String>>,
    allowed: impl Fn(&str, &str) -> bool,
) -> Option<Vec<String>> {
    match (parent, asked) {
        (Some(p), Some(a)) => Some(
            a.iter()
                .filter(|x| p.iter().any(|y| allowed(y, x)))
                .cloned()
                .collect(),
        ),
        (p, a) => a.clone().or_else(|| p.clone()),
    }
}

fn min_budget(a: &Option<Budget>, b: &Option<Budget>) -> Option<Budget> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Budget {
            tokens: min_opt(a.tokens, b.tokens),
            cost: match (&a.cost, &b.cost) {
                (Some(x), Some(y)) if x.currency == y.currency => Some(Cost {
                    amount: x.amount.min(y.amount),
                    currency: x.currency.clone(),
                }),
                (x, y) => x.clone().or_else(|| y.clone()),
            },
            seconds: min_opt(a.seconds, b.seconds),
            turns: min_opt(a.turns, b.turns),
            sessions: min_opt(a.sessions, b.sessions),
        }),
        (a, b) => a.clone().or_else(|| b.clone()),
    }
}

/// A child's ceiling: the parent's intersected with the one asked for (spec 22).
pub fn intersect(parent: &Ceiling, asked: &Ceiling) -> Ceiling {
    Ceiling {
        level: min_opt(parent.level, asked.level),
        network: match (parent.network, asked.network) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(true) && b.unwrap_or(true)),
        },
        isolation: max_opt(parent.isolation, asked.isolation),
        effects: lists(&parent.effects, &asked.effects, |p, a| p == a),
        functions: lists(&parent.functions, &asked.functions, glob),
        agents: lists(&parent.agents, &asked.agents, |p, a| p == a),
        envs: lists(&parent.envs, &asked.envs, |p, a| p == a),
        paths: lists(&parent.paths, &asked.paths, |p, a| inside(a, p)),
        depth: min_opt(parent.depth.map(|d| d.saturating_sub(1)), asked.depth),
        children: min_opt(parent.children, asked.children),
        budget: min_budget(&parent.budget, &asked.budget),
        unattended: min_opt(parent.unattended, asked.unattended),
    }
}

/// Whether `child` allows nothing `parent` doesn't. Depth narrows by one level per generation.
pub fn narrows(child: &Ceiling, parent: &Ceiling) -> bool {
    let le = |c: Option<u32>, p: Option<u32>| p.is_none_or(|p| c.is_some_and(|c| c <= p));
    let list_ok = |c: &Option<Vec<String>>, p: &Option<Vec<String>>, allowed: &dyn Fn(&str, &str) -> bool| {
        p.as_ref().is_none_or(|p| {
            c.as_ref()
                .is_some_and(|c| c.iter().all(|x| p.iter().any(|y| allowed(y, x))))
        })
    };
    parent.level.is_none_or(|p| child.level.is_some_and(|c| c <= p))
        && (parent.network != Some(false) || child.network == Some(false))
        && parent
            .isolation
            .is_none_or(|p| child.isolation.is_some_and(|c| c >= p))
        && list_ok(&child.effects, &parent.effects, &|p, a| p == a)
        && list_ok(&child.functions, &parent.functions, &glob)
        && list_ok(&child.agents, &parent.agents, &|p, a| p == a)
        && list_ok(&child.envs, &parent.envs, &|p, a| p == a)
        && list_ok(&child.paths, &parent.paths, &|p, a| inside(a, p))
        && le(child.depth, parent.depth.map(|d| d.saturating_sub(1)))
        && le(child.children, parent.children)
        && parent
            .unattended
            .is_none_or(|p| child.unattended.is_some_and(|c| c <= p))
}

/// The ceiling over a session (its own, which already narrows its ancestors').
pub fn of(state: &State, session: &str) -> Ceiling {
    state
        .tree
        .nodes
        .get(session)
        .map(|n| n.ceiling.clone())
        .unwrap_or_default()
}

/// The ceiling of the principal behind a connection: a session's own, else none.
pub fn of_conn(state: &State, conn: &str) -> Ceiling {
    match crate::principals::session_principal(state, conn) {
        Some(s) => of(state, s),
        None => Ceiling::default(),
    }
}

/// `-32003` with a reason.
pub fn forbidden(reason: &str, detail: impl Into<String>) -> parrot_proto::rpc::RpcError {
    parrot_proto::rpc::RpcError::new(-32003, detail.into()).with_data(json!({ "reason": reason }))
}

/// Whether an agent and environment are allowed under a ceiling (spec 22).
pub fn check_agent(ceiling: &Ceiling, agent: &str, env: &str) -> Result<(), parrot_proto::rpc::RpcError> {
    if ceiling
        .agents
        .as_ref()
        .is_some_and(|a| !a.iter().any(|x| x == agent))
    {
        return Err(forbidden(
            "ceiling",
            format!("agent {agent} is outside the ceiling"),
        ));
    }
    if ceiling.envs.as_ref().is_some_and(|e| !e.iter().any(|x| x == env)) {
        return Err(forbidden(
            "ceiling",
            format!("environment {env} is outside the ceiling"),
        ));
    }
    Ok(())
}

/// Whether a workspace path is inside a ceiling's `paths` (spec 8 fences).
pub fn check_path(ceiling: &Ceiling, path: &str) -> Result<(), parrot_proto::rpc::RpcError> {
    if ceiling
        .paths
        .as_ref()
        .is_some_and(|ps| !ps.iter().any(|p| inside(path, p)))
    {
        return Err(forbidden(
            "ceiling",
            format!("{path} is outside the ceiling's paths"),
        ));
    }
    Ok(())
}

/// A request opened under a ceiling with `unattended`: deny it once the time passes with nobody
/// able to answer (spec 21).
pub fn on_request(cx: &mut Cx, request: &str, session: &str) {
    if let Some(secs) = of(cx.state, session).unattended {
        let at_ms = cx.now_ms + secs * 1000;
        cx.state.ceilings.unattended.insert(request.to_string(), at_ms);
        cx.emit(crate::Effect::TimerAt { at_ms });
    }
}

/// Whether some connection could answer a session's requests: a person, not a session principal.
fn answerable(state: &State, session: &str) -> bool {
    state.principals.conns.iter().any(|(conn, c)| {
        !matches!(c.peer, crate::Peer::Session { .. })
            && crate::principals::allows(state, conn, Scope::Answer)
            && crate::principals::sees(state, conn, session)
    })
}

/// Time passed: deny unattended requests that are due.
pub fn tick(cx: &mut Cx) {
    let now = cx.now_ms;
    let due: Vec<String> = cx
        .state
        .ceilings
        .unattended
        .iter()
        .filter(|(_, at)| **at <= now)
        .map(|(id, _)| id.clone())
        .collect();
    for id in due {
        cx.state.ceilings.unattended.remove(&id);
        let Some(rec) = cx.state.requests.by_id.get(&id) else {
            continue;
        };
        if rec.request.status != RequestStatus::Open || rec.pending.is_some() {
            continue;
        }
        if answerable(cx.state, &rec.request.session) {
            continue;
        }
        let deny = rec.request.choices.as_ref().and_then(|cs| {
            cs.iter()
                .find(|c| c.intent == Some(parrot_proto::request::ChoiceIntent::Deny))
                .map(|c| c.id.clone())
        });
        let Some(deny) = deny else {
            continue;
        };
        let answer = parrot_proto::request::Answer {
            choice: Some(deny),
            message: Some("nobody could answer before the ceiling's unattended time".into()),
            ..Default::default()
        };
        crate::requests::answer_as_runtime(cx, &id, answer, "ceiling");
    }
}

/// The ceiling JSON for `Session.ceiling`.
pub fn json_of(c: &Ceiling) -> Value {
    serde_json::to_value(c).unwrap_or_else(|_| json!({}))
}
