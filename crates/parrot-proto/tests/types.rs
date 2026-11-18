//! Round trips of the spec's shapes, and the Appendix E checklist.

use parrot_proto::action::*;
use parrot_proto::agent::*;
use parrot_proto::driver::*;
use parrot_proto::env::*;
use parrot_proto::handshake::*;
use parrot_proto::item::*;
use parrot_proto::open::Open;
use parrot_proto::option::*;
use parrot_proto::prompt::*;
use parrot_proto::request::*;
use parrot_proto::session::*;
use parrot_proto::state::*;
use parrot_proto::subscribe::*;
use parrot_proto::usage::*;
use parrot_proto::workspace::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Parse `v` as `T`, write it back, and require the same JSON.
fn rt<T: DeserializeOwned + Serialize + std::fmt::Debug>(v: Value) -> T {
    let t: T =
        serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{}: {e}", std::any::type_name::<T>()));
    assert_eq!(
        serde_json::to_value(&t).unwrap(),
        v,
        "{}",
        std::any::type_name::<T>()
    );
    t
}

fn origin() -> Value {
    json!({ "kind": "client", "principal": "con_01J0000000000000000000000A" })
}

fn option() -> Value {
    json!({
        "id": "model", "role": "model", "label": "Model",
        "schema": { "oneOf": [{ "const": "m1", "title": "M1" }] },
        "value": "m1", "default": "m1", "applies": "next_turn"
    })
}

fn action() -> Value {
    json!({
        "id": "compact", "name": "/compact", "label": "Compact", "scope": "session",
        "when": ["idle"], "effects": ["turn"], "source": "harness",
        "input": { "type": "object" }, "requires": ["terminal"]
    })
}

fn agent() -> Value {
    json!({
        "id": "claude", "env": "env_01J0000000000000000000000A", "name": "Claude",
        "driver": { "name": "claude", "version": "1.0.0", "kind": "builtin", "data": "3" },
        "harness": { "version": "2.0.1", "verified": true },
        "installed": true, "auth": "signed_in", "usable": true,
        "controls": ["native", "tui"], "features": { "session.steer": { "when": "next_step" } },
        "levels": ["read", "write", "sandboxed", "host"], "isolation": ["none", "sandbox"],
        "options": [option()], "actions": [action()]
    })
}

fn env() -> Value {
    json!({
        "id": "env_01J0000000000000000000000A", "runtime": "rt_01J0000000000000000000000A",
        "name": "laptop", "os": "linux", "arch": "x86_64", "status": "online",
        "roots": ["/home/ada"], "features": { "git": {} }
    })
}

fn workspace() -> Value {
    json!({
        "id": "wsp_01J0000000000000000000000A", "env": "env_01J0000000000000000000000A",
        "kind": "worktree", "path": "/home/ada/src/app-wt", "repo": "/home/ada/src/app",
        "branch": "parrot/ses_1", "base": "main", "owner": "ses_01J0000000000000000000000A",
        "sessions": [], "created": "2026-01-01T00:00:00Z"
    })
}

fn binding() -> Value {
    json!({
        "id": "bnd_1", "session": "ses_1", "agent": "claude", "env": "env_1", "control": "native",
        "ref": { "id": "abc", "strength": "strong" }, "isolation": "sandbox", "level": "write",
        "network": false, "status": "running", "via": "new", "pending": "bundle",
        "started": "2026-01-01T00:00:00Z"
    })
}

fn session() -> Value {
    json!({
        "id": "ses_1", "runtime": "rt_1", "title": "Fix tests", "workspace": "wsp_1",
        "cwd": "/home/ada/src/app", "state": "idle",
        "background": [{ "id": "b1", "kind": "x.acme/job", "title": "Indexing" }],
        "binding": binding(), "bindings": ["bnd_1"], "kind": "child",
        "parent": { "session": "ses_0", "item": "itm_1" },
        "task": { "status": "running" }, "origin": origin(), "grant": ["read", "prompt"],
        "ceiling": { "paths": ["/home/ada"] }, "advisory": false,
        "budget": { "limit": { "tokens": 1000 }, "used": { "tokens": 10 } },
        "mcp": [{ "name": "parrot", "status": "needs_auth", "tools": 3 }],
        "created": "2026-01-01T00:00:00Z", "updated": "2026-01-01T00:00:01Z", "archived": false
    })
}

fn summary() -> Value {
    json!({
        "id": "ses_1", "runtime": "rt_1", "state": "running", "kind": "root", "agent": "claude",
        "task": "running", "archived": false, "updated": "2026-01-01T00:00:00Z"
    })
}

fn turn() -> Value {
    json!({
        "id": "trn_1", "binding": "bnd_1", "status": "interrupted", "stop": "max_tokens",
        "continuable": true, "input": "msg_1", "started": "2026-01-01T00:00:00Z",
        "ended": "2026-01-01T00:00:05Z",
        "error": { "code": "overloaded", "message": "try later", "retryable": true },
        "usage": { "tokens": { "input": 10, "output": 5, "cacheRead": 2 } }
    })
}

fn request() -> Value {
    json!({
        "id": "req_1", "session": "ses_1", "path": ["ses_0", "ses_1"], "turn": "trn_1",
        "kind": "approval", "title": "Run npm test?",
        "subject": { "type": "command", "command": "npm test", "cwd": "/w" },
        "level": "sandboxed", "network": true,
        "choices": [{ "id": "y", "label": "Allow", "intent": "allow", "scope": "once" }],
        "blocking": true, "answerable": "connection", "notify": "urgent", "status": "open"
    })
}

#[test]
fn handshake() {
    rt::<InitializeParams>(json!({
        "protocol": [1], "peer": { "kind": "client", "name": "t3", "version": "0.1.0" },
        "features": { "client.forms": {} }, "provides": [{ "name": "x" }], "_meta": { "a": 1 }
    }));
    rt::<InitializeResult>(json!({
        "protocol": 1, "peer": { "kind": "runtime", "name": "parrot", "version": "0.1.0" },
        "runtime": { "id": "rt_1", "env": "env_1" }, "features": { "profile.core": {} },
        "connection": { "id": "con_1", "principal": "con_1", "scopes": ["read", "functions"], "sessions": ["ses_1"] }
    }));
}

#[test]
fn agents_and_envs() {
    rt::<Agent>(agent());
    rt::<AgentListResult>(json!({ "agents": [agent()] }));
    rt::<AgentListParams>(json!({ "env": "env_1" }));
    rt::<AgentRefreshParams>(json!({ "agent": "claude" }));
    rt::<AgentRefreshResult>(json!({ "agent": agent() }));
    rt::<Environment>(env());
    rt::<EnvListResult>(json!({ "envs": [env()] }));
    rt::<EnvListParams>(json!({}));
}

#[test]
fn driver_agent_has_no_env() {
    let mut a = agent();
    a.as_object_mut().unwrap().remove("env");
    let parsed: Agent = rt(a);
    assert!(parsed.env.is_empty());
}

#[test]
fn workspaces() {
    rt::<Workspace>(workspace());
    rt::<WorkspaceCreateParams>(
        json!({ "env": "env_1", "request": { "kind": "worktree", "repo": "/r", "base": "main" } }),
    );
    rt::<WorkspaceCreateParams>(
        json!({ "env": "env_1", "request": { "kind": "use", "workspace": "wsp_1" }, "key": "k" }),
    );
    rt::<WorkspaceRequest>(json!({ "kind": "dir", "path": "/r" }));
    rt::<WorkspaceCreateResult>(json!({ "workspace": workspace() }));
    rt::<WorkspaceListParams>(json!({ "env": "env_1", "cursor": "c1", "limit": 10 }));
    rt::<WorkspaceListResult>(json!({ "workspaces": [workspace()], "next": "c2" }));
    rt::<WorkspaceStatusParams>(json!({ "workspace": "wsp_1" }));
    rt::<WorkspaceStatusResult>(
        json!({ "branch": "main", "commit": "abc", "ahead": 1, "behind": 0, "dirty": true, "changed": ["a.rs"] }),
    );
    rt::<WorkspaceMergeParams>(
        json!({ "from": "wsp_2", "into": "wsp_1", "method": "squash", "message": "m" }),
    );
    rt::<WorkspaceMergeResult>(json!({ "conflicts": ["a.rs"] }));
    rt::<WorkspaceTransferParams>(
        json!({ "workspace": "wsp_1", "to": "env_2", "include": ["commits", "untracked"] }),
    );
    rt::<WorkspaceRemoveParams>(json!({ "workspace": "wsp_1", "force": true }));
    rt::<WorkspaceRemoveResult>(json!({}));
}

#[test]
fn sessions() {
    rt::<Session>(session());
    rt::<SessionSummary>(summary());
    rt::<Binding>(binding());
    rt::<SessionNewParams>(json!({
        "agent": "claude", "workspace": { "kind": "dir", "path": "/w" }, "options": { "model": "m1" },
        "control": "tui", "start": false, "grant": ["read"], "parent": "ses_0",
        "task": { "goal": "x" }, "seed": { "from": "ses_0" }, "ceiling": {}, "wake": { "on": ["done"] },
        "budget": { "turns": 3 }, "onParentEnd": "stop", "key": "k1"
    }));
    rt::<SessionNewResult>(json!({ "session": session(), "subscription": "sub_1", "head": "c42" }));
    rt::<SessionListParams>(json!({ "parent": "ses_0", "archived": false, "cursor": "c", "limit": 5 }));
    rt::<SessionListResult>(json!({ "sessions": [summary()], "next": "c" }));
    rt::<SessionGetParams>(json!({ "session": "ses_1" }));
    rt::<SessionGetResult>(json!({ "session": session() }));
    rt::<SessionTreeParams>(json!({ "session": "ses_1", "cursor": "c" }));
    rt::<SessionTreeResult>(json!({ "nodes": [{ "session": summary(), "depth": 1 }], "next": "c" }));
    rt::<SessionStartParams>(json!({ "session": "ses_1", "options": {}, "key": "k" }));
    rt::<SessionStartResult>(json!({ "binding": binding() }));
    rt::<SessionStopParams>(json!({ "session": "ses_1", "tree": true }));
    rt::<SessionRebindParams>(
        json!({ "session": "ses_1", "agent": "codex", "env": "env_2", "require": "exact", "interrupt": true }),
    );
    rt::<SessionRebindResult>(json!({ "binding": binding() }));
    rt::<SessionRewindParams>(json!({ "session": "ses_1", "turn": "trn_1", "files": true }));
    rt::<SessionRenameParams>(json!({ "session": "ses_1", "title": "t" }));
    rt::<SessionArchiveParams>(json!({ "session": "ses_1", "archived": true }));
    rt::<SessionDeleteParams>(json!({ "session": "ses_1" }));
    rt::<SessionDiscoverParams>(json!({ "agent": "claude", "cwd": "/w", "limit": 3 }));
    rt::<SessionDiscoverResult>(
        json!({ "found": [{ "ref": { "id": "h1", "strength": "weak" }, "cwd": "/w", "updated": "2026-01-01T00:00:00Z" }] }),
    );
    rt::<SessionAdoptParams>(
        json!({ "agent": "claude", "ref": { "id": "h1", "strength": "strong" }, "start": true }),
    );
    rt::<SessionAdoptResult>(json!({ "session": session(), "subscription": "sub_1", "head": "c1" }));
}

#[test]
fn state_and_turns() {
    for s in [
        "starting",
        "idle",
        "running",
        "requires_action",
        "stopped",
        "lost",
        "unrecognised",
        "unknown",
    ] {
        rt::<State>(json!(s));
    }
    rt::<Turn>(turn());
    let t: Turn = rt(
        json!({ "id": "trn_2", "binding": "bnd_1", "status": "failed", "stop": "x.acme/oom", "continuable": false, "started": "2026-01-01T00:00:00Z" }),
    );
    assert!(matches!(t.stop, Some(Open::Unknown(_))));
}

#[test]
fn item_patches() {
    rt::<ItemPatch>(json!({
        "id": "itm_1", "kind": "assistant", "turn": "trn_1", "binding": "bnd_1", "source": "native",
        "origin": { "kind": "harness" }, "set": { "text": "", "_meta": { "a": 1 } }, "streaming": true
    }));
    rt::<ItemPatch>(json!({ "id": "itm_1", "append": { "text": "hello" } }));
    rt::<ItemPatch>(json!({ "id": "itm_1", "streaming": false, "removed": true }));
    let p: ItemPatch = rt(json!({ "id": "itm_2", "kind": "x.acme/blob" }));
    assert!(matches!(p.kind, Some(Open::Unknown(_))));
}

#[test]
fn item_kinds() {
    let c = |kind: &str, fields: Value| {
        let mut v = json!({ "id": "itm_1", "kind": kind, "turn": "trn_1", "origin": origin() });
        v.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let item: Item = rt(v);
        let body = item.known().unwrap_or_else(|| panic!("{kind} not known"));
        assert_eq!(body.common().id, "itm_1");
    };
    c(
        "user",
        json!({ "text": "hi", "context": [{ "uri": "file:///a", "text": "a" }], "attachments": [{ "name": "a.png", "type": "image/png", "data": "AA==" }], "status": "sent" }),
    );
    c("assistant", json!({ "text": "**ok**" }));
    c("reasoning", json!({ "text": "hm", "summary": true }));
    c(
        "tool",
        json!({ "tool": "Read", "title": "Read a.rs", "input": { "path": "a.rs" }, "status": "paused", "output": "x", "server": "fs", "caller": "script", "function": "parrot.x" }),
    );
    c(
        "script",
        json!({ "language": "python", "code": "print(1)", "status": "ok", "background": false, "result": 1 }),
    );
    c(
        "command",
        json!({ "command": "ls", "cwd": "/w", "status": "error", "output": "no", "exit": 2 }),
    );
    c(
        "file_change",
        json!({ "changes": [{ "path": "/w/b", "op": "move", "from": "/w/a", "diff": "" }], "status": "ok" }),
    );
    c(
        "plan",
        json!({ "entries": [{ "text": "step", "status": "active" }] }),
    );
    c(
        "subagent",
        json!({ "title": "helper", "session": "ses_2", "status": "running" }),
    );
    c(
        "web",
        json!({ "query": "rust", "status": "cancelled", "results": [] }),
    );
    c("review", json!({ "target": "HEAD", "findings": [] }));
    c("compaction", json!({ "trigger": "auto", "summary": "s" }));
    c(
        "hook",
        json!({ "event": "PreToolUse", "name": "lint", "outcome": "blocked" }),
    );
    c(
        "handoff",
        json!({ "from": "bnd_1", "to": "bnd_2", "via": "bundle" }),
    );
    c("error", json!({ "message": "boom", "code": "E1" }));
}

#[test]
fn unknown_item_kind_and_fields_survive() {
    let raw = json!({ "id": "itm_9", "kind": "x.acme/diagram", "title": "D", "svg": "<svg/>" });
    let item: Item = rt(raw);
    assert!(matches!(item, Open::Unknown(_)));
    let known: Item = rt(
        json!({ "id": "itm_1", "kind": "assistant", "text": "t", "x.acme/score": 3, "_meta": { "k": true } }),
    );
    let Open::Known(ItemBody::Assistant(a)) = known else {
        panic!()
    };
    assert_eq!(a.extra["x.acme/score"], 3);
    assert!(a.common.meta.is_some());
}

#[test]
fn prompts_and_queue() {
    rt::<SessionPromptParams>(json!({
        "session": "ses_1", "text": "go", "context": [{ "uri": "file:///a", "title": "a", "text": "x" }],
        "attachments": [{ "name": "f", "type": "text/plain", "uri": "file:///f" }], "delivery": "interrupt", "key": "k"
    }));
    rt::<SessionPromptResult>(json!({ "message": "msg_1", "disposition": "queued", "queued": "q_1" }));
    rt::<SessionCancelParams>(json!({ "session": "ses_1", "tree": true }));
    rt::<SessionCancelResult>(json!({ "interrupted": true, "queued": 2 }));
    rt::<QueueRemoveParams>(json!({ "session": "ses_1", "id": "q_1" }));
    rt::<QueueEditParams>(json!({ "session": "ses_1", "id": "q_1", "text": "t" }));
    rt::<QueueMoveParams>(json!({ "session": "ses_1", "id": "q_1", "index": 0 }));
    rt::<QueueClearParams>(json!({ "session": "ses_1" }));
    rt::<QueueResumeParams>(json!({ "session": "ses_1" }));
}

#[test]
fn e4_queue_entry_kinds() {
    let q: QueueListResult = rt(json!({ "queue": { "paused": true, "entries": [
        { "id": "q_1", "message": "msg_1", "text": "a", "origin": origin(), "kind": "prompt" },
        { "id": "q_2", "text": "/compact", "origin": origin(), "kind": "action" },
        { "id": "q_3", "text": "child done", "origin": { "kind": "wake" }, "kind": "wake" }
    ] } }));
    let kinds: Vec<_> = q.queue.entries.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            QueueEntryKind::Prompt,
            QueueEntryKind::Action,
            QueueEntryKind::Wake
        ]
    );
}

#[test]
fn requests() {
    rt::<Request>(request());
    rt::<Request>(json!({
        "id": "req_2", "session": "ses_1", "path": ["ses_1"], "kind": "question", "title": "Which?",
        "input": { "type": "object", "properties": { "a": { "oneOf": [{ "const": "x", "title": "X" }] } } },
        "level": "host", "blocking": false, "answerable": "message", "expires": "2026-01-02T00:00:00Z",
        "fallback": { "choice": "n" }, "notify": "none", "status": "answered",
        "answer": { "input": { "a": "x" }, "message": "thanks" }, "answeredBy": "ceiling"
    }));
    for s in [
        json!({ "type": "tool", "tool": "Bash", "input": {}, "editable": true }),
        json!({ "type": "files", "paths": ["/a"], "diff": "d" }),
        json!({ "type": "network", "host": "example.com" }),
        json!({ "type": "plan", "text": "p" }),
        json!({ "type": "function", "function": "parrot.x", "input": 1 }),
    ] {
        let s: Open<Subject> = rt(s);
        assert!(s.known().is_some());
    }
    let s: Open<Subject> = rt(json!({ "type": "x.acme/robot", "arm": 2 }));
    assert!(matches!(s, Open::Unknown(_)));
    rt::<RequestAnswerParams>(
        json!({ "request": "req_1", "answer": { "choice": "y", "value": true }, "key": "k" }),
    );
    rt::<RequestListParams>(json!({ "tree": "ses_0", "status": "open", "cursor": "c" }));
    rt::<RequestListResult>(json!({ "requests": [request()], "next": "c" }));
    let r: Request = rt({
        let mut r = request();
        r["kind"] = json!("x.acme/captcha");
        r["x.acme/hint"] = json!("h");
        r
    });
    assert!(matches!(r.kind, Open::Unknown(_)));
    assert_eq!(r.extra["x.acme/hint"], "h");
}

#[test]
fn e8_answerable_only_connection_or_message() {
    rt::<Answerable>(json!("connection"));
    rt::<Answerable>(json!("message"));
    assert!(serde_json::from_value::<Answerable>(json!("process")).is_err());
    let mut r = request();
    r["answerable"] = json!("process");
    assert!(serde_json::from_value::<Request>(r).is_err());
}

#[test]
fn options_and_actions() {
    rt::<ConfigOption>(option());
    let o: ConfigOption = rt(json!({
        "id": "x", "role": "x.acme/temperature", "label": "T", "description": "d",
        "schema": { "type": "number" }, "value": 0.5, "applies": "start",
        "dependsOn": { "option": "model", "schemas": { "m1": { "type": "number" } } }, "complete": false,
        "x.acme/unit": "K"
    }));
    assert!(matches!(o.role, Some(Open::Unknown(_))));
    for l in ["read", "write", "sandboxed", "host"] {
        rt::<PermissionLevel>(json!(l));
    }
    assert!(PermissionLevel::Read < PermissionLevel::Host);
    rt::<OptionSetParams>(json!({ "session": "ses_1", "values": { "model": "m2" }, "key": "k" }));
    rt::<OptionSetResult>(json!({ "options": [option()], "reset": ["effort"], "level": "write" }));
    rt::<OptionCompleteParams>(json!({ "agent": "claude", "option": "model", "prefix": "m", "limit": 5 }));
    rt::<OptionCompleteResult>(json!({ "values": [{ "const": "m1", "title": "M1" }] }));
    rt::<OptionListParams>(json!({ "session": "ses_1" }));
    rt::<OptionListResult>(json!({ "options": [option()], "level": "read", "network": false }));
    rt::<Action>(action());
    let a: Action = rt(
        json!({ "id": "x", "label": "X", "scope": "agent", "when": ["idle", "running"], "effects": ["read", "write", "destructive"], "source": "x.acme/plugin" }),
    );
    assert!(matches!(a.source, Open::Unknown(_)));
    rt::<ActionInvokeParams>(
        json!({ "session": "ses_1", "action": "continue", "input": { "note": "n" }, "delivery": "queue", "key": "k" }),
    );
    rt::<ActionInvokeParams>(json!({ "agent": "claude", "env": "env_1", "action": "login", "args": "--x" }));
    rt::<ActionInvokeResult>(json!({ "status": "requested", "request": "req_1" }));
    rt::<ActionInvokeResult>(json!({ "status": "done", "message": "msg_1", "result": { "text": "s" } }));
    rt::<ActionListParams>(json!({ "session": "ses_1" }));
    rt::<ActionListResult>(json!({ "actions": [action()] }));
}

#[test]
fn usage_and_budgets() {
    rt::<Usage>(json!({
        "tokens": { "input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4, "reasoning": 5 },
        "cost": { "amount": 0.25, "currency": "USD" }, "context": { "used": 10, "window": 200000 },
        "rateLimits": [{ "name": "5h", "used": 0.5, "limit": 1.0, "resets": "2026-01-01T05:00:00Z" }],
        "countedIn": "ses_0"
    }));
    rt::<BudgetState>(json!({
        "limit": { "tokens": 100, "cost": { "amount": 1.0, "currency": "USD" }, "seconds": 60, "turns": 3, "sessions": 2 },
        "used": { "tokens": 1 }
    }));
}

#[test]
fn subscriptions() {
    rt::<SubscribeParams>(json!({ "target": { "session": "ses_1" }, "items": 20 }));
    rt::<SubscribeParams>(json!({ "target": { "tree": "ses_1" }, "replayFrom": "c17" }));
    let p: SubscribeParams = rt(json!({ "target": { "index": true } }));
    assert_eq!(p.target, Target::Index { index: true });
    rt::<SubscribeResult>(json!({ "subscription": "sub_1", "head": "c1", "snapshot": {
        "sessions": [{
            "session": session(),
            "items": [{ "id": "itm_1", "kind": "assistant", "text": "hi" }],
            "before": "c0", "requests": [request()],
            "queue": { "paused": false, "entries": [] }, "options": [option()], "actions": [action()],
            "usage": { "cost": { "amount": 0.0, "currency": "USD" } },
            "subscribers": [{ "id": "con_1", "name": "t3", "principal": "con_1", "device": "phone" }]
        }],
        "index": { "sessions": [summary()], "next": "c", "agents": [agent()], "envs": [env()] }
    } }));
    rt::<UnsubscribeParams>(json!({ "subscription": "sub_1" }));
    rt::<SessionReadParams>(json!({ "session": "ses_1", "cursor": "c", "direction": "back", "limit": 10 }));
    rt::<SessionReadResult>(
        json!({ "items": [{ "id": "itm_1", "kind": "x.acme/new" }], "before": "a", "after": "b" }),
    );
    rt::<ItemGetParams>(json!({ "session": "ses_1", "item": "itm_1" }));
    rt::<ItemGetResult>(json!({ "item": { "id": "itm_1", "kind": "error", "message": "m" } }));
}

#[test]
fn updates() {
    let us = [
        json!({ "type": "state", "state": "running", "background": [], "detail": "d" }),
        json!({ "type": "session", "session": session() }),
        json!({ "type": "item", "item": { "id": "itm_1", "append": { "text": "x" } } }),
        json!({ "type": "turn", "turn": turn() }),
        json!({ "type": "request", "request": request() }),
        json!({ "type": "queue", "queue": { "paused": false, "entries": [] } }),
        json!({ "type": "options", "options": [option()], "level": "host", "network": true }),
        json!({ "type": "actions", "actions": [action()] }),
        json!({ "type": "usage", "usage": {} }),
        json!({ "type": "notice", "level": "warning", "text": "t", "code": "c" }),
        json!({ "type": "summary", "session": summary() }),
        json!({ "type": "agent", "agent": agent() }),
        json!({ "type": "env", "env": env() }),
        json!({ "type": "workspace", "workspace": workspace() }),
        json!({ "type": "deleted", "kind": "workspace", "id": "wsp_1" }),
    ];
    for u in us {
        let p: UpdateParams = rt(
            json!({ "subscription": "sub_1", "seq": "c9", "runtime": "rt_1", "at": "2026-01-01T00:00:00Z", "session": "ses_1", "update": u }),
        );
        assert!(p.update.known().is_some(), "{:?}", p.update);
    }
}

#[test]
fn unknown_update_type_survives() {
    let p: UpdateParams = rt(json!({
        "subscription": "sub_1", "seq": "c9", "runtime": "rt_1", "at": "2026-01-01T00:00:00Z",
        "update": { "type": "x.acme/ping", "n": 1 }, "x.acme/trace": "t"
    }));
    assert!(matches!(p.update, Open::Unknown(_)));
    assert_eq!(p.extra["x.acme/trace"], "t");
}

#[test]
fn e2_updates_carry_runtime_and_opaque_cursors() {
    let p: UpdateParams = rt(
        json!({ "subscription": "sub_1", "seq": "rt_1:00042", "runtime": "rt_2", "at": "2026-01-01T00:00:00Z", "update": { "type": "usage", "usage": {} } }),
    );
    assert_eq!(p.runtime, "rt_2");
    assert_eq!(p.seq, "rt_1:00042");
    let missing = json!({ "subscription": "sub_1", "seq": "c", "at": "2026-01-01T00:00:00Z", "update": { "type": "usage", "usage": {} } });
    assert!(serde_json::from_value::<UpdateParams>(missing).is_err());
    let s: SubscribeParams = rt(json!({ "target": { "session": "ses_1" }, "replayFrom": "opaque/abc==" }));
    assert_eq!(s.replay_from.as_deref(), Some("opaque/abc=="));
    let r: SessionNewResult = rt(json!({ "session": session(), "subscription": "sub_1", "head": "opaque" }));
    assert_eq!(r.head, "opaque");
    assert!(
        serde_json::from_value::<SubscribeParams>(json!({ "target": { "session": "s" }, "replayFrom": 5 }))
            .is_err()
    );
}

#[test]
fn e7_interrupted_task_status() {
    let s: SessionSummary = rt({
        let mut s = summary();
        s["task"] = json!("interrupted");
        s
    });
    assert_eq!(s.task, Some(Open::Known(TaskStatus::Interrupted)));
}

#[test]
fn unknown_fields_survive() {
    let mut s = session();
    s["x.acme/color"] = json!("red");
    s["_meta"] = json!({ "trace": "t" });
    let parsed: Session = rt(s);
    assert_eq!(parsed.extra["x.acme/color"], "red");
    let mut a = agent();
    a["x.acme/x"] = json!(1);
    rt::<Agent>(a);
    let mut w = workspace();
    w["x.acme/x"] = json!([1]);
    rt::<Workspace>(w);
    let mut e = env();
    e["x.acme/x"] = json!(null);
    rt::<Environment>(e);
    rt::<parrot_proto::ids::Empty>(json!({ "x.acme/x": 1 }));
}

#[test]
fn driver_methods() {
    rt::<BindingStart>(json!({
        "session": "ses_1", "binding": "bnd_1", "agent": "claude", "workspace": "/w", "cwd": "/w",
        "options": { "model": "m1" }, "control": "native", "resume": { "id": "h1", "strength": "strong" },
        "pin": { "level": "write", "network": false }, "isolation": "sandbox",
        "deny": ["/run/parrot/sock", "/var/lib/parrot"],
        "mcp": { "command": ["parrot", "mcp"], "token": "tok_test" }
    }));
    rt::<BindingStartResult>(
        json!({ "ref": { "id": "h1", "strength": "weak" }, "isolation": "none", "level": "host", "network": true, "options": [option()], "actions": [action()] }),
    );
    rt::<BindingStopParams>(json!({ "binding": "bnd_1" }));
    rt::<BindingPromptParams>(
        json!({ "binding": "bnd_1", "subagent": "sa1", "message": "msg_1", "text": "go", "steer": false }),
    );
    rt::<BindingPromptResult>(json!({ "disposition": "steered" }));
    rt::<BindingCancelParams>(json!({ "binding": "bnd_1" }));
    rt::<BindingCancelResult>(json!({ "interrupted": false }));
    rt::<BindingAnswerParams>(json!({ "binding": "bnd_1", "request": "k1", "answer": { "choice": "y" } }));
    rt::<BindingOptionsParams>(json!({ "binding": "bnd_1", "values": { "mode": "plan" } }));
    rt::<BindingOptionsResult>(json!({ "options": [], "level": "read", "network": false }));
    rt::<BindingActionParams>(
        json!({ "binding": "bnd_1", "action": "compact", "message": "msg_2", "input": {} }),
    );
    rt::<BindingActionResult>(json!({ "status": "started" }));
    rt::<BindingForkParams>(json!({ "binding": "bnd_1", "at": "t3" }));
    rt::<BindingForkResult>(json!({ "ref": { "id": "h2", "strength": "strong" } }));
    rt::<HistoryDiscoverParams>(json!({ "agent": "claude", "cursor": "c" }));
    rt::<HistoryDiscoverResult>(json!({ "found": [], "next": "c" }));
    rt::<HistoryReadParams>(
        json!({ "agent": "claude", "ref": { "id": "h1", "strength": "strong" }, "cursor": "c" }),
    );
    rt::<BindingScreenParams>(json!({ "binding": "bnd_1" }));
    rt::<BindingScreenResult>(
        json!({ "text": "$ ", "cursor": { "row": 0, "col": 2 }, "size": { "rows": 24, "cols": 80 } }),
    );
    rt::<BindingKeysParams>(json!({ "binding": "bnd_1", "text": "y", "press": ["Enter"] }));
    rt::<DriverAgentListParams>(json!({}));
}

#[test]
fn driver_updates() {
    let us = [
        json!({ "type": "state", "state": "idle", "background": [] }),
        json!({ "type": "item", "item": { "id": "k1", "kind": "subagent", "set": { "key": "sa1", "title": "helper" } } }),
        json!({ "type": "turn", "turn": { "id": "t1", "status": "running", "continuable": false, "started": "2026-01-01T00:00:00Z" } }),
        json!({ "type": "request", "request": { "id": "k2", "kind": "auth", "title": "Trust folder?", "level": "host", "blocking": true, "answerable": "connection", "notify": "normal", "status": "open" } }),
        json!({ "type": "options", "options": [], "level": "write", "network": false }),
        json!({ "type": "actions", "actions": [] }),
        json!({ "type": "usage", "usage": { "context": { "used": 1, "window": 2 } } }),
        json!({ "type": "ref", "ref": { "id": "h1", "strength": "strong" } }),
        json!({ "type": "mcp", "servers": [{ "name": "parrot", "status": "connected" }] }),
        json!({ "type": "notice", "level": "info", "text": "t" }),
        json!({ "type": "frame", "source": "stdout", "data": { "raw": "x" } }),
    ];
    for u in us {
        let d: DriverUpdate = rt(json!({ "binding": "bnd_1", "subagent": "sa1", "update": u }));
        assert!(d.update.known().is_some(), "{:?}", d.update);
    }
    let d: DriverUpdate = rt(json!({ "binding": "bnd_1", "update": { "type": "x.acme/new" } }));
    assert!(matches!(d.update, Open::Unknown(_)));
}

#[test]
fn e4_driver_user_item_carries_message() {
    let item: Item =
        rt(json!({ "id": "k1", "kind": "user", "text": "go", "status": "sent", "message": "msg_1" }));
    let Open::Known(ItemBody::User(u)) = item else {
        panic!()
    };
    assert_eq!(u.message.as_deref(), Some("msg_1"));
}

#[test]
fn e4_history_read_uses_empty_binding() {
    let r: HistoryReadResult = rt(json!({ "updates": [
        { "binding": "", "update": { "type": "item", "item": { "id": "k1", "kind": "user", "set": { "text": "hi", "status": "sent" } } } },
        { "binding": "", "update": { "type": "turn", "turn": { "id": "t1", "status": "completed", "continuable": false, "started": "2026-01-01T00:00:00Z" } } }
    ], "next": "c" }));
    assert!(r.updates.iter().all(|u| u.binding.is_empty()));
    let s = serde_json::to_string(&r.updates[0]).unwrap();
    assert!(s.starts_with(r#"{"binding":"""#), "{s}");
}

#[test]
fn schemas_generate() {
    let _ = schemars::schema_for!(UpdateParams);
    let _ = schemars::schema_for!(DriverUpdate);
    let _ = schemars::schema_for!(Item);
    let _ = schemars::schema_for!(SessionSnapshot);
}
