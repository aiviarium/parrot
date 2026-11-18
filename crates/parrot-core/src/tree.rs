//! The session tree (spec 18, 22, Appendix E8): kinds, children, ceilings, `session/tree`,
//! `session/configure`, and the worktree workspaces children work in (spec 8).

use crate::budgets::Allotment;
use crate::ceilings::{self, Ceiling};
use crate::principals::{check, check_session, parse, session_principal};
use crate::sessions::{WorkspaceRecord, timestamp};
use crate::tasks::{Task, TaskRequest};
use crate::wakes::Wake;
use crate::{CallOwner, Cx, Reply, State};
use parrot_proto::principal::Scope;
use parrot_proto::rpc::{RequestId, RpcError};
use parrot_proto::session::SessionNewParams;
use parrot_proto::state::State as SessionState;
use parrot_proto::workspace::{
    MergeMethod, WorkspaceCreateParams, WorkspaceMergeParams, WorkspaceRemoveParams, WorkspaceRequest,
    WorkspaceStatusParams,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What the tree keeps per session and per workspace.
#[derive(Debug, Default)]
pub struct Tree {
    pub nodes: BTreeMap<String, Node>,
    /// What `sessions.workspaces` doesn't keep: kind, repository, branch, base and owner.
    pub workspaces: BTreeMap<String, WorkspaceMeta>,
    /// Calls waiting on a workspace operation.
    pub calls: BTreeMap<u64, Pending>,
}

/// A session's place in the tree.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// `root`, `child` or `embedded`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_item: Option<String>,
    #[serde(default)]
    pub ceiling: Ceiling,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<Wake>,
    /// `continue` or `stop`.
    #[serde(default = "continue_")]
    pub on_parent_end: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Allotment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
    /// `Seed` (spec 23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<Value>,
}

fn continue_() -> String {
    "continue".into()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMeta {
    /// `dir`, `worktree` or `external`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// What a workspace operation's answer continues.
#[derive(Debug, Clone)]
pub enum Pending {
    /// `session/new` with a new worktree.
    New {
        conn: String,
        id: Option<RequestId>,
        params: Box<SessionNewParams>,
        plan: Box<Plan>,
        session: String,
        workspace: String,
        meta: WorkspaceMeta,
    },
    /// `workspace/create` with a worktree.
    Create {
        conn: String,
        id: Option<RequestId>,
        workspace: String,
        meta: WorkspaceMeta,
    },
    /// A call answered with the operation's result as it is.
    Answer { conn: String, id: Option<RequestId> },
    /// `workspace/merge`: answered with the result, or refused when `into` is dirty.
    Merge { conn: String, id: Option<RequestId> },
    /// `workspace/remove`: forgets the workspace once git removed it.
    Remove {
        conn: String,
        id: Option<RequestId>,
        workspace: String,
    },
    /// Nothing waits on it.
    Ignore,
}

/// What `session/new` checked for a child, seed or task before the session exists.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub node: Node,
    pub parent: Option<String>,
    pub task: Option<TaskRequest>,
    pub seed: Option<crate::seeds::SeedPlan>,
}

/// Persist a session's node.
pub fn persist(cx: &mut Cx, session: &str) {
    if let Some(n) = cx.state.tree.nodes.get(session) {
        let v = serde_json::to_value(n).unwrap_or_default();
        cx.persist(json!({ "op": "put", "kind": "tree", "id": session, "value": v }));
    }
}

/// Put back a node read from the store.
pub fn restore(state: &mut State, session: &str, value: &Value) {
    if let Ok(n) = serde_json::from_value::<Node>(value.clone()) {
        state.tree.nodes.insert(session.to_string(), n);
    }
}

/// Put back what a stored workspace record says beyond its path.
pub fn restore_workspace(state: &mut State, id: &str, value: &Value) {
    let s = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
    state.tree.workspaces.insert(
        id.to_string(),
        WorkspaceMeta {
            kind: s("kind").unwrap_or_else(|| "dir".into()),
            repo: s("repo"),
            branch: s("branch"),
            base: s("base"),
            owner: s("owner"),
        },
    );
}

/// Add the tree's fields to a `Session` JSON.
pub fn decorate(state: &State, id: &str, v: &mut Value) {
    let Some(n) = state.tree.nodes.get(id) else {
        return;
    };
    v["kind"] = json!(n.kind);
    v["ceiling"] = ceilings::json_of(&n.ceiling);
    if let Some(item) = &n.parent_item
        && v.get("parent").is_some()
    {
        v["parent"]["item"] = json!(item);
    }
    if let Some(t) = &n.task {
        v["task"] = json!(t);
    }
    if let Some(b) = &n.budget {
        v["budget"] = crate::budgets::json_of(b);
    }
    if let Some(s) = &n.seed {
        v["seed"] = s.clone();
    }
    if let Some(w) = &n.wake {
        v["wake"] = json!(w);
    }
    v["onParentEnd"] = json!(n.on_parent_end);
}

/// Add the tree's fields to a `SessionSummary` JSON.
pub fn decorate_summary(state: &State, id: &str, v: &mut Value) {
    let Some(n) = state.tree.nodes.get(id) else {
        return;
    };
    v["kind"] = json!(n.kind);
    if let Some(t) = &n.task {
        v["task"] = json!(t.status);
    }
}

/// The children of a session.
pub fn children(state: &State, session: &str) -> Vec<String> {
    state
        .sessions
        .by_id
        .values()
        .filter(|r| r.parent.as_deref() == Some(session))
        .map(|r| r.id.clone())
        .collect()
}

/// A session and all its descendants, depth first, with their depth below it.
pub fn subtree_depths(state: &State, root: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_string(), 0u32)];
    while let Some((s, d)) = stack.pop() {
        if out.iter().any(|(x, _)| x == &s) {
            continue;
        }
        let mut kids = children(state, &s);
        kids.sort();
        for k in kids.into_iter().rev() {
            stack.push((k, d + 1));
        }
        out.push((s, d));
    }
    out
}

/// A session and all its descendants.
pub fn subtree(state: &State, root: &str) -> Vec<String> {
    subtree_depths(state, root).into_iter().map(|(s, _)| s).collect()
}

/// Whether a session counts as running for the `children` limit.
fn running(state: &State, session: &str) -> bool {
    crate::sessions::active(state, session)
}

/// The workspace JSON (spec 8).
pub fn workspace_json(state: &State, id: &str) -> Option<Value> {
    let w = state.sessions.workspaces.get(id)?;
    let meta = state
        .tree
        .workspaces
        .get(id)
        .cloned()
        .unwrap_or_else(|| WorkspaceMeta {
            kind: "dir".into(),
            ..Default::default()
        });
    let sessions: Vec<&String> = state
        .sessions
        .info
        .iter()
        .filter(|(_, i)| i.workspace == id)
        .map(|(s, _)| s)
        .collect();
    let mut v = json!({ "id": id, "env": state.principals.env, "kind": meta.kind, "path": w.path,
        "sessions": sessions, "created": timestamp(w.created_ms) });
    for (k, x) in [
        ("repo", &meta.repo),
        ("branch", &meta.branch),
        ("base", &meta.base),
        ("owner", &meta.owner),
    ] {
        if let Some(x) = x {
            v[k] = json!(x);
        }
    }
    Some(v)
}

/// Register a workspace and publish it to the index.
pub fn add_workspace(cx: &mut Cx, id: &str, path: &str, meta: WorkspaceMeta) {
    cx.state.sessions.workspaces.insert(
        id.to_string(),
        WorkspaceRecord {
            id: id.to_string(),
            path: path.to_string(),
            created_ms: cx.now_ms,
        },
    );
    cx.state.tree.workspaces.insert(id.to_string(), meta);
    publish_workspace(cx, id);
}

pub fn publish_workspace(cx: &mut Cx, id: &str) {
    if let Some(w) = workspace_json(cx.state, id) {
        cx.persist(json!({ "op": "put", "kind": "workspace", "id": id, "value": w }));
        cx.publish(None, json!({ "type": "workspace", "workspace": w }));
    }
}

/// Where a new worktree for `workspace` of `repo` goes: beside the repository, never inside it.
pub fn worktree_path(repo: &str, workspace: &str) -> String {
    let repo = repo.trim_end_matches('/');
    let (dir, name) = match repo.rsplit_once('/') {
        Some((d, n)) => (d.to_string(), n.to_string()),
        None => (".".to_string(), repo.to_string()),
    };
    format!("{dir}/.parrot-worktrees/{name}/{workspace}")
}

/// Whether a workspace is a worktree.
pub fn is_worktree(state: &State, workspace: &str) -> bool {
    state
        .tree
        .workspaces
        .get(workspace)
        .is_some_and(|m| m.kind == "worktree")
}

/// Check `session/new` for a child, task or seed, and plan the node (spec 18, 19, 22, E9).
pub fn plan_new(cx: &mut Cx, conn: &str, p: &SessionNewParams) -> Result<Plan, RpcError> {
    check(cx.state, conn, Scope::Delegate)?;
    let mut plan = Plan::default();
    let (parent_ceiling, depth_left) = match &p.parent {
        Some(parent) => {
            check_session(cx.state, conn, Scope::Delegate, parent)?;
            let pc = ceilings::of(cx.state, parent);
            if pc.depth == Some(0) {
                return Err(ceilings::forbidden(
                    "depth",
                    "the parent's ceiling allows no more children",
                ));
            }
            if let Some(max) = pc.children {
                let n = children(cx.state, parent)
                    .iter()
                    .filter(|c| running(cx.state, c))
                    .count() as u32;
                if n >= max {
                    return Err(ceilings::forbidden(
                        "children",
                        format!("the parent already runs {n} children"),
                    ));
                }
            }
            (pc, true)
        }
        None => {
            check(cx.state, conn, Scope::Manage)?;
            (ceilings::of_conn(cx.state, conn), false)
        }
    };
    let _ = depth_left;
    let asked: Ceiling = match &p.ceiling {
        Some(c) => parse(c.clone())?,
        None => Ceiling::default(),
    };
    let ceiling = if p.parent.is_some() {
        ceilings::intersect(&parent_ceiling, &asked)
    } else {
        // A root's ceiling narrows the creating principal's without spending a level.
        let mut c = ceilings::intersect(&parent_ceiling, &asked);
        c.depth = match (parent_ceiling.depth, asked.depth) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        c
    };
    ceilings::check_agent(
        &ceiling,
        &p.agent,
        p.env.as_deref().unwrap_or(&cx.state.principals.env),
    )?;
    if matches!(p.workspace, WorkspaceRequest::Worktree { .. }) {
        // Spawning with a new worktree needs `workspace` (E8).
        check(cx.state, conn, Scope::Workspace)?;
    }
    let budget = match &p.parent {
        Some(parent) => crate::budgets::allot(cx.state, parent, p.budget.as_ref())?,
        None => p
            .budget
            .clone()
            .or_else(|| ceiling.budget.clone())
            .map(|b| Allotment {
                limit: b,
                ..Default::default()
            }),
    };
    let wake: Option<Wake> = match &p.wake {
        Some(w) => Some(parse(w.clone())?),
        None => None,
    };
    if let Some(t) = &p.task {
        plan.task = Some(parse(t.clone())?);
    }
    plan.node = Node {
        kind: if p.parent.is_some() { "child" } else { "root" }.into(),
        parent_item: p
            .extra
            .get("parentItem")
            .and_then(Value::as_str)
            .map(str::to_string),
        ceiling,
        wake,
        on_parent_end: p
            .on_parent_end
            .map(|o| {
                if o == parrot_proto::session::OnParentEnd::Stop {
                    "stop"
                } else {
                    "continue"
                }
            })
            .unwrap_or("continue")
            .into(),
        budget,
        task: None,
        seed: None,
    };
    plan.parent = p.parent.clone();
    Ok(plan)
}

/// Undo what `plan_new` took when `session/new` fails later: the allotment.
pub fn unplan(cx: &mut Cx, plan: &Plan) {
    if let (Some(parent), Some(a)) = (&plan.parent, &plan.node.budget)
        && let Some(pa) = cx
            .state
            .tree
            .nodes
            .get_mut(parent)
            .and_then(|n| n.budget.as_mut())
    {
        crate::budgets::sub(&mut pa.allotted, &a.limit);
        crate::budgets::sub(
            &mut pa.used,
            &parrot_proto::usage::Budget {
                sessions: Some(1),
                ..Default::default()
            },
        );
    }
}

/// Attach a planned node to a session that now exists.
pub fn attach(cx: &mut Cx, session: &str, plan: &Plan) {
    cx.state.tree.nodes.insert(session.to_string(), plan.node.clone());
    if let Some(rec) = cx.state.sessions.by_id.get_mut(session) {
        rec.parent = plan.parent.clone();
    }
    persist(cx, session);
    if let Some(parent) = &plan.parent {
        // The parent has a `subagent` item for the child (spec 18.1).
        let item = crate::bindings::derive_id("itm", &[parent, "subagent", session]);
        let title = cx.state.sessions.info.get(session).and_then(|i| i.title.clone());
        let mut set = json!({ "session": session, "status": "running" });
        if let Some(t) = title {
            set["title"] = json!(t);
        }
        cx.publish(
            Some(parent.clone()),
            json!({ "type": "item", "item": { "id": item, "kind": "subagent", "source": "runtime", "set": set } }),
        );
        persist(cx, parent);
        crate::sessions::changed(cx, parent);
    }
}

/// Make sure every session has a node (roots made before the tree existed).
pub fn ensure(state: &mut State, session: &str) {
    if !state.tree.nodes.contains_key(session) {
        let kind = if state
            .sessions
            .by_id
            .get(session)
            .and_then(|r| r.parent.as_ref())
            .is_some()
        {
            "child"
        } else {
            "root"
        };
        state.tree.nodes.insert(
            session.to_string(),
            Node {
                kind: kind.into(),
                on_parent_end: continue_(),
                ..Default::default()
            },
        );
    }
}

/// Handle `session/tree`, `session/configure` and `workspace/*`.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "session/tree" => Reply::Now(tree(cx, conn, params)),
        "session/configure" => Reply::Now(configure(cx, conn, params)),
        "workspace/list" => Reply::Now(list_workspaces(cx, conn, params)),
        "workspace/create" => later(create(cx, conn, id, params)),
        "workspace/status" => later(status(cx, conn, id, params)),
        "workspace/merge" => later(merge(cx, conn, id, params)),
        "workspace/remove" => later(remove(cx, conn, id, params)),
        "workspace/transfer" => Reply::Now(Err(
            RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "profile.link" }))
        )),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn later(r: Result<Option<Value>, RpcError>) -> Reply {
    match r {
        Ok(Some(v)) => Reply::Now(Ok(v)),
        Ok(None) => Reply::Later,
        Err(e) => Reply::Now(Err(e)),
    }
}

fn tree(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let p: parrot_proto::session::SessionTreeParams = parse(params)?;
    check_session(cx.state, conn, Scope::Read, &p.session)?;
    let all = subtree_depths(cx.state, &p.session);
    let offset: usize = match &p.cursor {
        Some(c) => c.parse().map_err(|_| RpcError::new(-32602, "invalid cursor"))?,
        None => 0,
    };
    let limit = p.limit.unwrap_or(100).clamp(1, 1000) as usize;
    let nodes: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(limit)
        .filter_map(|(s, d)| {
            crate::sessions::summary_json(cx.state, s).map(|v| json!({ "session": v, "depth": d }))
        })
        .collect();
    let mut v = json!({ "nodes": nodes });
    if offset + limit < all.len() {
        v["next"] = json!((offset + limit).to_string());
    }
    Ok(v)
}

/// `session/configure` (E8): `delegate` on a subtree, `manage` on a root. Ceilings only narrow.
fn configure(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    let session = params
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "invalid params: session"))?
        .to_string();
    let is_root = cx
        .state
        .sessions
        .by_id
        .get(&session)
        .is_some_and(|r| r.parent.is_none());
    check_session(
        cx.state,
        conn,
        if is_root { Scope::Manage } else { Scope::Delegate },
        &session,
    )?;
    if session_principal(cx.state, conn) == Some(session.as_str()) {
        return Err(ceilings::forbidden("subtree", "a session can't configure itself"));
    }
    ensure(cx.state, &session);
    if let Some(c) = params.get("ceiling") {
        let asked: Ceiling = parse(c.clone())?;
        let n = cx.state.tree.nodes.get_mut(&session).expect("ensured");
        let mut next = ceilings::intersect(&n.ceiling, &asked);
        // Narrowing one's own ceiling doesn't spend a level.
        next.depth = match (n.ceiling.depth, asked.depth) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        n.ceiling = next.clone();
        // Descendants keep within it.
        for d in subtree(cx.state, &session).into_iter().skip(1) {
            if let Some(dn) = cx.state.tree.nodes.get_mut(&d) {
                let mut c = ceilings::intersect(&next, &dn.ceiling);
                c.depth = dn.ceiling.depth.map(|x| x.min(next.depth.unwrap_or(u32::MAX)));
                dn.ceiling = c;
            }
            persist(cx, &d);
        }
    }
    if let Some(w) = params.get("wake") {
        let wake: Wake = parse(w.clone())?;
        cx.state.tree.nodes.get_mut(&session).expect("ensured").wake = Some(wake);
    }
    if let Some(o) = params.get("onParentEnd").and_then(Value::as_str) {
        if o != "stop" && o != "continue" {
            return Err(RpcError::new(-32602, "invalid params: onParentEnd"));
        }
        cx.state
            .tree
            .nodes
            .get_mut(&session)
            .expect("ensured")
            .on_parent_end = o.into();
    }
    if let Some(b) = params.get("budget") {
        let b: parrot_proto::usage::Budget = parse(b.clone())?;
        let n = cx.state.tree.nodes.get_mut(&session).expect("ensured");
        match &mut n.budget {
            // Only lowering: raising would take more from the parent than it gave.
            Some(a) if !is_root => {
                let ok = [
                    (b.tokens, a.limit.tokens),
                    (b.turns, a.limit.turns),
                    (b.seconds, a.limit.seconds),
                    (b.sessions, a.limit.sessions),
                ]
                .iter()
                .all(|(new, old)| old.is_none_or(|o| new.is_some_and(|n| n <= o)));
                if !ok {
                    return Err(ceilings::forbidden(
                        "budget",
                        "a child's budget can only be lowered",
                    ));
                }
                a.limit = b;
            }
            Some(a) => a.limit = b,
            None if is_root => {
                n.budget = Some(Allotment {
                    limit: b,
                    ..Default::default()
                })
            }
            None => {
                return Err(ceilings::forbidden(
                    "budget",
                    "the session has no allotment to change",
                ));
            }
        }
    }
    persist(cx, &session);
    crate::sessions::changed(cx, &session);
    Ok(json!({}))
}

fn list_workspaces(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    check(cx.state, conn, Scope::Read)?;
    let session = params.get("session").and_then(Value::as_str);
    let ids: Vec<String> = cx.state.sessions.workspaces.keys().cloned().collect();
    let list: Vec<Value> = ids
        .iter()
        .filter(|w| session.is_none_or(|s| cx.state.sessions.info.get(s).is_some_and(|i| &i.workspace == *w)))
        .filter(|w| visible_workspace(cx.state, conn, w))
        .filter_map(|w| workspace_json(cx.state, w))
        .collect();
    Ok(json!({ "workspaces": list }))
}

/// Whether a session principal may touch a workspace: one its subtree uses or owns.
/// Whether a connection may see a workspace: a session principal sees its subtree's, a token
/// limited to some sessions sees only theirs (E8), everyone else sees all.
pub fn visible_workspace(state: &State, conn: &str, workspace: &str) -> bool {
    let Some(me) = session_principal(state, conn) else {
        if !token_limited(state, conn) {
            return true;
        }
        let owner = state
            .tree
            .workspaces
            .get(workspace)
            .and_then(|m| m.owner.as_deref());
        return owner.is_some_and(|o| crate::principals::sees(state, conn, o))
            || state
                .sessions
                .info
                .iter()
                .any(|(s, i)| i.workspace == workspace && crate::principals::sees(state, conn, s));
    };
    let mine = subtree(state, me);
    let owner = state.tree.workspaces.get(workspace).and_then(|m| m.owner.clone());
    owner.is_some_and(|o| mine.contains(&o))
        || mine.iter().any(|s| {
            state
                .sessions
                .info
                .get(s)
                .is_some_and(|i| i.workspace == workspace)
        })
}

/// Whether the connection holds a token limited to some sessions.
pub fn token_limited(state: &State, conn: &str) -> bool {
    matches!(
        state.principals.conns.get(conn).map(|c| &c.peer),
        Some(crate::Peer::Token {
            sessions: Some(_),
            ..
        })
    )
}

fn own_workspace(state: &State, me: &str, workspace: &str) -> bool {
    state
        .sessions
        .info
        .get(me)
        .is_some_and(|i| i.workspace == workspace)
        || state
            .tree
            .workspaces
            .get(workspace)
            .and_then(|m| m.owner.as_deref())
            == Some(me)
}

fn not_found(id: &str) -> RpcError {
    RpcError::new(-32002, format!("workspace not found: {id}"))
}

/// Start creating a worktree; `pending` continues when it exists.
pub fn create_worktree(
    cx: &mut Cx,
    repo: &str,
    branch: &str,
    base: Option<&str>,
    workspace: &str,
    pending: Pending,
) {
    let path = worktree_path(repo, workspace);
    let call = cx.call_workspace(
        CallOwner::Tasks,
        "worktree.create",
        json!({ "repo": repo, "path": path, "branch": branch, "base": base }),
    );
    cx.state.tree.calls.insert(call, pending);
}

fn create(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Result<Option<Value>, RpcError> {
    let p: WorkspaceCreateParams = parse(params)?;
    check(cx.state, conn, Scope::Workspace)?;
    // A new workspace belongs to no session yet, so a session-limited token can't make one (E8).
    if token_limited(cx.state, conn) {
        return Err(RpcError::new(-32003, "forbidden").with_data(json!({ "reason": "sessions" })));
    }
    if p.env != cx.state.principals.env {
        return Err(RpcError::new(-32001, "unsupported").with_data(json!({ "feature": "profile.link" })));
    }
    let ceiling = ceilings::of_conn(cx.state, conn);
    let owner = session_principal(cx.state, conn).map(str::to_string);
    match p.request {
        WorkspaceRequest::Dir { path } => {
            ceilings::check_path(&ceiling, &path)?;
            let existing = cx
                .state
                .sessions
                .workspaces
                .values()
                .find(|w| w.path == path)
                .map(|w| w.id.clone());
            let wid = match existing {
                Some(w) => w,
                None => {
                    let wid = cx.ids.next("wsp");
                    add_workspace(
                        cx,
                        &wid,
                        &path,
                        WorkspaceMeta {
                            kind: "dir".into(),
                            owner,
                            ..Default::default()
                        },
                    );
                    wid
                }
            };
            Ok(Some(json!({ "workspace": workspace_json(cx.state, &wid) })))
        }
        WorkspaceRequest::Use { workspace } => {
            if !cx.state.sessions.workspaces.contains_key(&workspace) {
                return Err(not_found(&workspace));
            }
            Ok(Some(json!({ "workspace": workspace_json(cx.state, &workspace) })))
        }
        WorkspaceRequest::Worktree { repo, branch, base } => {
            ceilings::check_path(&ceiling, &repo)?;
            let wid = cx.ids.next("wsp");
            let branch =
                branch.unwrap_or_else(|| format!("parrot/{}", owner.clone().unwrap_or_else(|| wid.clone())));
            let meta = WorkspaceMeta {
                kind: "worktree".into(),
                repo: Some(repo.clone()),
                branch: Some(branch.clone()),
                base: base.clone(),
                owner,
            };
            create_worktree(
                cx,
                &repo,
                &branch,
                base.as_deref(),
                &wid,
                Pending::Create {
                    conn: conn.to_string(),
                    id: id.cloned(),
                    workspace: wid.clone(),
                    meta,
                },
            );
            Ok(None)
        }
    }
}

fn status(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Result<Option<Value>, RpcError> {
    let p: WorkspaceStatusParams = parse(params)?;
    check(cx.state, conn, Scope::Read)?;
    let w = cx
        .state
        .sessions
        .workspaces
        .get(&p.workspace)
        .filter(|_| visible_workspace(cx.state, conn, &p.workspace))
        .ok_or_else(|| not_found(&p.workspace))?
        .clone();
    let meta = cx
        .state
        .tree
        .workspaces
        .get(&p.workspace)
        .cloned()
        .unwrap_or_default();
    let call = cx.call_workspace(
        CallOwner::Tasks,
        "status",
        json!({ "path": w.path, "kind": meta.kind, "base": meta.base }),
    );
    cx.state.tree.calls.insert(
        call,
        Pending::Answer {
            conn: conn.to_string(),
            id: id.cloned(),
        },
    );
    Ok(None)
}

fn merge(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Result<Option<Value>, RpcError> {
    let p: WorkspaceMergeParams = parse(params)?;
    check(cx.state, conn, Scope::Workspace)?;
    let path = |w: &str| {
        cx.state
            .sessions
            .workspaces
            .get(w)
            .map(|r| r.path.clone())
            .ok_or_else(|| not_found(w))
    };
    let (from, into) = (path(&p.from)?, path(&p.into)?);
    if !visible_workspace(cx.state, conn, &p.from) || !visible_workspace(cx.state, conn, &p.into) {
        return Err(not_found(&p.from));
    }
    // `from` is the caller's own or a descendant's; `into` is its own (E8).
    if let Some(me) = session_principal(cx.state, conn).map(str::to_string) {
        if !visible_workspace(cx.state, conn, &p.from) {
            return Err(ceilings::forbidden(
                "subtree",
                "from is not a workspace of the caller's subtree",
            ));
        }
        if !own_workspace(cx.state, &me, &p.into) {
            return Err(ceilings::forbidden(
                "subtree",
                "into is not the caller's workspace",
            ));
        }
    }
    let busy = cx.state.sessions.info.iter().any(|(_, i)| {
        i.workspace == p.into && matches!(i.state, SessionState::Running | SessionState::RequiresAction)
    });
    if busy {
        return Err(RpcError::new(-32004, "a turn runs in the target workspace")
            .with_data(json!({ "state": "running" })));
    }
    let method = match p.method {
        MergeMethod::Merge => "merge",
        MergeMethod::Squash => "squash",
    };
    let call = cx.call_workspace(
        CallOwner::Tasks,
        "merge",
        json!({ "from": from, "into": into, "method": method, "message": p.message }),
    );
    cx.state.tree.calls.insert(
        call,
        Pending::Merge {
            conn: conn.to_string(),
            id: id.cloned(),
        },
    );
    Ok(None)
}

fn remove(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Result<Option<Value>, RpcError> {
    let p: WorkspaceRemoveParams = parse(params)?;
    check(cx.state, conn, Scope::Workspace)?;
    let force = p.force.unwrap_or(false);
    if force {
        check(cx.state, conn, Scope::Destructive)?;
    }
    let w = cx
        .state
        .sessions
        .workspaces
        .get(&p.workspace)
        .filter(|_| visible_workspace(cx.state, conn, &p.workspace))
        .ok_or_else(|| not_found(&p.workspace))?
        .clone();
    let used: Vec<String> = cx
        .state
        .sessions
        .info
        .iter()
        .filter(|(s, i)| i.workspace == p.workspace && crate::sessions::active(cx.state, s))
        .map(|(s, _)| s.clone())
        .collect();
    if !used.is_empty() {
        return Err(
            RpcError::new(-32004, "sessions use the workspace").with_data(json!({ "sessions": used }))
        );
    }
    let meta = cx
        .state
        .tree
        .workspaces
        .get(&p.workspace)
        .cloned()
        .unwrap_or_default();
    if meta.kind != "worktree" {
        // `dir` and `external` workspaces are only forgotten (spec 8).
        forget_workspace(cx, &p.workspace);
        return Ok(Some(json!({})));
    }
    let call = cx.call_workspace(
        CallOwner::Tasks,
        "worktree.remove",
        json!({ "repo": meta.repo, "path": w.path, "branch": meta.branch, "base": meta.base, "force": force }),
    );
    cx.state.tree.calls.insert(
        call,
        Pending::Remove {
            conn: conn.to_string(),
            id: id.cloned(),
            workspace: p.workspace,
        },
    );
    Ok(None)
}

fn forget_workspace(cx: &mut Cx, id: &str) {
    cx.state.sessions.workspaces.remove(id);
    cx.state.tree.workspaces.remove(id);
    cx.persist(json!({ "op": "delete", "kind": "workspace", "id": id }));
    cx.publish(None, json!({ "type": "deleted", "kind": "workspace", "id": id }));
}

/// A workspace operation answered. Returns false when the call wasn't the tree's.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) -> bool {
    let Some(pending) = cx.state.tree.calls.remove(&call) else {
        return false;
    };
    let reply = |cx: &mut Cx, conn: &str, id: Option<RequestId>, r: Result<Value, RpcError>| {
        if let Some(id) = id {
            cx.reply(conn, id, r);
        }
    };
    match pending {
        Pending::Ignore => {}
        Pending::Answer { conn, id } => reply(cx, &conn, id, result),
        Pending::Merge { conn, id } => {
            let r = result.map_err(|e| {
                if e.message.contains("uncommitted") {
                    RpcError::new(-32004, e.message).with_data(json!({ "state": "dirty" }))
                } else {
                    e
                }
            });
            reply(cx, &conn, id, r);
        }
        Pending::Remove { conn, id, workspace } => {
            let r = result.map(|_| {
                forget_workspace(cx, &workspace);
                json!({})
            });
            reply(cx, &conn, id, r);
        }
        Pending::Create {
            conn,
            id,
            workspace,
            mut meta,
        } => match result {
            Ok(v) => {
                let path = v
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(b) = v.get("base").and_then(Value::as_str) {
                    meta.base = Some(b.to_string());
                }
                add_workspace(cx, &workspace, &path, meta);
                let w = workspace_json(cx.state, &workspace);
                reply(cx, &conn, id, Ok(json!({ "workspace": w })));
            }
            Err(e) => reply(cx, &conn, id, Err(e)),
        },
        Pending::New {
            conn,
            id,
            params,
            plan,
            session,
            workspace,
            mut meta,
        } => match result {
            Ok(v) => {
                let path = v
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(b) = v.get("base").and_then(Value::as_str) {
                    meta.base = Some(b.to_string());
                }
                add_workspace(cx, &workspace, &path, meta);
                crate::sessions::finish_new(
                    cx,
                    &conn,
                    id.as_ref(),
                    *params,
                    Some(*plan),
                    workspace,
                    path,
                    Some(session),
                );
            }
            Err(e) => {
                unplan(cx, &plan);
                reply(cx, &conn, id, Err(e));
            }
        },
    }
    true
}
