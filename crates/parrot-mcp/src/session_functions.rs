//! The session functions (spec 28): projections of protocol methods, called as the session's
//! principal on its subtree.

use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::catalog::{Access, key_schema};
use crate::rules;
use crate::upstream::Upstream;

/// What a session function needs before it is listed.
#[derive(Debug, PartialEq, Eq)]
pub enum Needs {
    Scope(&'static str),
    /// The caller has a task.
    Task,
}

#[derive(Debug)]
pub struct SessionFunction {
    pub name: &'static str,
    /// The protocol methods it projects.
    pub methods: &'static [&'static str],
    pub description: &'static str,
    pub effects: &'static [&'static str],
    pub needs: Needs,
    pub input: fn() -> Value,
    pub output: fn() -> Value,
}

impl SessionFunction {
    pub fn available(&self, access: &Access) -> bool {
        match self.needs {
            Needs::Scope(scope) => access.scopes.iter().any(|s| s == scope),
            Needs::Task => access.task,
        }
    }

    /// Whether it takes an idempotency key (spec 27.1).
    pub fn keyed(&self) -> bool {
        self.effects.iter().any(|e| *e != "read")
    }

    /// The input schema, with `key` added to functions that change something.
    pub fn input_schema(&self) -> Value {
        let mut schema = (self.input)();
        if self.keyed()
            && let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut)
        {
            props.insert("key".into(), key_schema());
        }
        schema
    }
}

/// Who calls, and how long a call may take.
#[derive(Clone)]
pub struct Context {
    pub upstream: Upstream,
    pub session: String,
    pub workspace: Option<String>,
    pub limit: Duration,
}

pub fn find(name: &str) -> Option<&'static SessionFunction> {
    ALL.iter().find(|f| f.name == name)
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn task_request_props() -> Map<String, Value> {
    let v = json!({
        "brief": { "type": "string", "description": "The child's first prompt" },
        "title": { "type": "string" },
        "result": { "type": "object", "description": "JSON Schema the child's output must match" },
        "completeOn": { "type": "string", "enum": ["finish", "idle"], "description": "finish: the child calls parrot__task__finish; idle: going idle completes the task. Default finish" },
    });
    v.as_object().cloned().unwrap_or_default()
}

fn task_schema() -> Value {
    obj(
        json!({
            "id": { "type": "string" },
            "status": { "type": "string", "enum": ["running", "awaiting", "done", "failed", "cancelled", "interrupted"] },
            "output": { "description": "Structured if the task had a result schema, else { text }" },
            "error": { "type": "string" },
        }),
        &["id", "status"],
    )
}

fn workspace_input() -> Value {
    json!({
        "description": "Where the child works. Default: the caller's workspace",
        "oneOf": [
            { "type": "object", "properties": { "kind": { "const": "use" }, "workspace": { "type": "string" } }, "required": ["kind", "workspace"] },
            { "type": "object", "properties": { "kind": { "const": "dir" }, "path": { "type": "string" } }, "required": ["kind", "path"] },
            { "type": "object", "properties": { "kind": { "const": "worktree" }, "repo": { "type": "string" }, "branch": { "type": "string" }, "base": { "type": "string" } }, "required": ["kind", "repo"] },
        ]
    })
}

fn summary_schema() -> Value {
    obj(
        json!({
            "id": { "type": "string" }, "title": { "type": "string" }, "state": { "type": "string" },
            "kind": { "type": "string" }, "parent": { "type": "string" }, "agent": { "type": "string" },
            "task": { "type": "string" },
        }),
        &["id", "state", "kind"],
    )
}

pub static ALL: &[SessionFunction] = &[
    SessionFunction {
        name: "parrot.sessions.spawn",
        methods: &["session/new"],
        description: "Start a child session with a task. Its requests stay with people; wait on it with parrot__tasks__wait.",
        effects: &["write", "turn"],
        needs: Needs::Scope("delegate"),
        input: || {
            let mut props = task_request_props();
            props.insert(
                "agent".into(),
                json!({ "type": "string", "description": "The agent id, from parrot__agents__list" }),
            );
            props.insert(
                "env".into(),
                json!({ "type": "string", "description": "Default: the caller's environment" }),
            );
            props.insert("workspace".into(), workspace_input());
            props.insert("wake".into(), json!({ "type": "array", "items": { "type": "string", "enum": ["done", "awaiting", "failed", "requires_action"] }, "description": "Task changes that wake the caller when idle" }));
            props.insert(
                "onParentEnd".into(),
                json!({ "type": "string", "enum": ["continue", "stop"], "default": "continue" }),
            );
            obj(Value::Object(props), &["agent", "brief"])
        },
        output: || {
            obj(
                json!({ "session": { "type": "string" }, "task": task_schema() }),
                &["session"],
            )
        },
    },
    SessionFunction {
        name: "parrot.sessions.send",
        methods: &["session/prompt"],
        description: "Send a prompt to a session in the caller's subtree.",
        effects: &["turn"],
        needs: Needs::Scope("prompt"),
        input: || {
            obj(
                json!({
                    "session": { "type": "string" },
                    "text": { "type": "string" },
                    "delivery": { "type": "string", "enum": ["auto", "queue", "steer", "interrupt"], "default": "auto" },
                }),
                &["session", "text"],
            )
        },
        output: || {
            obj(
                json!({
                    "message": { "type": "string" },
                    "disposition": { "type": "string", "enum": ["started", "steered", "queued", "handled"] },
                    "queued": { "type": "string" },
                }),
                &["message", "disposition"],
            )
        },
    },
    SessionFunction {
        name: "parrot.sessions.act",
        methods: &["action/invoke"],
        description: "Invoke an action on a session in the caller's subtree, such as continue.",
        effects: &["turn", "write"],
        needs: Needs::Scope("prompt"),
        input: || {
            obj(
                json!({
                    "session": { "type": "string" },
                    "action": { "type": "string" },
                    "input": { "type": "object", "default": {} },
                }),
                &["session", "action"],
            )
        },
        output: || {
            obj(
                json!({
                    "status": { "type": "string", "enum": ["done", "started", "queued", "requested"] },
                    "message": { "type": "string" }, "request": { "type": "string" }, "result": {},
                }),
                &["status"],
            )
        },
    },
    SessionFunction {
        name: "parrot.sessions.cancel",
        methods: &["session/cancel"],
        description: "Interrupt a session's turn without stopping it; pauses its queue.",
        effects: &["turn"],
        needs: Needs::Scope("prompt"),
        input: || {
            obj(
                json!({ "session": { "type": "string" }, "tree": { "type": "boolean", "default": false } }),
                &["session"],
            )
        },
        output: || {
            obj(
                json!({ "interrupted": { "type": "boolean" }, "queued": { "type": "integer" } }),
                &["interrupted", "queued"],
            )
        },
    },
    SessionFunction {
        name: "parrot.sessions.stop",
        methods: &["session/stop"],
        description: "Stop a session's harness. Its log and workspace stay; a running task is cancelled.",
        effects: &["write"],
        needs: Needs::Scope("manage"),
        input: || {
            obj(
                json!({ "session": { "type": "string" }, "tree": { "type": "boolean", "default": false } }),
                &["session"],
            )
        },
        output: || obj(json!({}), &[]),
    },
    SessionFunction {
        name: "parrot.sessions.read",
        methods: &["session/get", "session/read"],
        description: "Read sessions and their newest items. Results are keyed by session, with a per-entry error.",
        effects: &["read"],
        needs: Needs::Scope("read"),
        input: || {
            obj(
                json!({
                    "sessions": { "type": "array", "items": { "type": "string" }, "description": "Session ids; default the caller" },
                    "limit": { "type": "integer", "minimum": 0, "maximum": 200, "default": 20, "description": "Items per session" },
                    "cursor": { "type": "string", "description": "An entry's before, to read older items; only with one session" },
                    "fields": { "type": "array", "items": { "type": "string" }, "description": "Top-level session fields to return; default all" },
                }),
                &[],
            )
        },
        output: || {
            obj(
                json!({
                    "results": { "type": "object", "description": "By session id: { session, items, before } or { error }" },
                    "next": { "type": "array", "items": { "type": "string" }, "description": "Sessions left out to stay under 256 KiB; read them next" },
                }),
                &["results"],
            )
        },
    },
    SessionFunction {
        name: "parrot.sessions.tree",
        methods: &["session/tree"],
        description: "List the caller's subtree.",
        effects: &["read"],
        needs: Needs::Scope("read"),
        input: || {
            obj(
                json!({ "cursor": { "type": "string" }, "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 } }),
                &[],
            )
        },
        output: || {
            obj(
                json!({
                    "nodes": { "type": "array", "items": obj(json!({ "session": summary_schema(), "depth": { "type": "integer" } }), &["session", "depth"]) },
                    "next": { "type": "string" },
                }),
                &["nodes"],
            )
        },
    },
    SessionFunction {
        name: "parrot.sessions.handoff",
        methods: &["session/rebind"],
        description: "Hand the caller over to another agent or environment. Returns at once; the outcome arrives as an update.",
        effects: &["write"],
        needs: Needs::Scope("delegate"),
        input: || {
            obj(
                json!({ "agent": { "type": "string" }, "env": { "type": "string" } }),
                &[],
            )
        },
        output: || obj(json!({ "binding": { "type": "object" } }), &[]),
    },
    SessionFunction {
        name: "parrot.tasks.assign",
        methods: &["task/assign"],
        description: "Give a child a new task after its last one ended; the brief is prompted.",
        effects: &["turn"],
        needs: Needs::Scope("delegate"),
        input: || {
            let mut props = task_request_props();
            props.insert("session".into(), json!({ "type": "string" }));
            obj(Value::Object(props), &["session", "brief"])
        },
        output: || obj(json!({ "task": task_schema() }), &["task"]),
    },
    SessionFunction {
        name: "parrot.tasks.wait",
        methods: &["task/wait"],
        description: "Wait for task changes of the named sessions. Returns at once if there are changes, else on the next one or after timeout. Call again with the returned cursor; nothing is lost between calls.",
        effects: &["read"],
        needs: Needs::Scope("read"),
        input: || {
            obj(
                json!({
                    "sessions": { "type": "array", "items": { "type": "string" } },
                    "cursor": { "type": ["string", "integer"], "description": "The cursor of the previous call; omit for the current state" },
                    "timeout": { "type": "integer", "minimum": 0, "description": "Seconds; capped at the harness's tool timeout" },
                }),
                &["sessions"],
            )
        },
        output: || {
            obj(
                json!({
                    "changes": { "type": "array", "items": obj(json!({ "session": { "type": "string" }, "task": task_schema() }), &["session", "task"]) },
                    "cursor": { "type": ["string", "integer"] },
                    "timedOut": { "type": "boolean" },
                }),
                &["changes", "cursor", "timedOut"],
            )
        },
    },
    SessionFunction {
        name: "parrot.task.finish",
        methods: &["task/finish"],
        description: "Finish the caller's task with its output. An output that doesn't match the task's result schema returns invalid_input; fix it and call again.",
        effects: &["write"],
        needs: Needs::Task,
        input: || {
            obj(
                json!({ "output": { "description": "Matches the task's result schema, else { text }" } }),
                &["output"],
            )
        },
        output: || obj(json!({}), &[]),
    },
    SessionFunction {
        name: "parrot.task.fail",
        methods: &["task/fail"],
        description: "Fail the caller's task with a reason.",
        effects: &["write"],
        needs: Needs::Task,
        input: || obj(json!({ "error": { "type": "string" } }), &["error"]),
        output: || obj(json!({}), &[]),
    },
    SessionFunction {
        name: "parrot.requests.list",
        methods: &["request/list"],
        description: "List the requests of the caller's subtree. Only people answer them.",
        effects: &["read"],
        needs: Needs::Scope("read"),
        input: || {
            obj(
                json!({ "status": { "type": "string", "enum": ["open", "any"], "default": "open" }, "cursor": { "type": "string" } }),
                &[],
            )
        },
        output: || {
            obj(
                json!({ "requests": { "type": "array", "items": { "type": "object" } }, "next": { "type": "string" } }),
                &["requests"],
            )
        },
    },
    SessionFunction {
        name: "parrot.workspaces.status",
        methods: &["workspace/status"],
        description: "The git status of a workspace.",
        effects: &["read"],
        needs: Needs::Scope("read"),
        input: || {
            obj(
                json!({ "workspace": { "type": "string", "description": "Default: the caller's workspace" } }),
                &[],
            )
        },
        output: || {
            obj(
                json!({
                    "branch": { "type": "string" }, "commit": { "type": "string" },
                    "ahead": { "type": "integer" }, "behind": { "type": "integer" },
                    "dirty": { "type": "boolean" }, "changed": { "type": "array", "items": { "type": "string" } },
                }),
                &["dirty", "changed"],
            )
        },
    },
    SessionFunction {
        name: "parrot.workspaces.merge",
        methods: &["workspace/merge"],
        description: "Merge a descendant's workspace into the caller's.",
        effects: &["write"],
        needs: Needs::Scope("workspace"),
        input: || {
            obj(
                json!({
                    "from": { "type": "string" },
                    "into": { "type": "string", "description": "Default: the caller's workspace" },
                    "method": { "type": "string", "enum": ["merge", "squash"], "default": "merge" },
                    "message": { "type": "string" },
                }),
                &["from"],
            )
        },
        output: || {
            obj(
                json!({ "commit": { "type": "string" }, "conflicts": { "type": "array", "items": { "type": "string" } } }),
                &[],
            )
        },
    },
    SessionFunction {
        name: "parrot.agents.list",
        methods: &["agent/list"],
        description: "List the agents sessions can be spawned with.",
        effects: &["read"],
        needs: Needs::Scope("read"),
        input: || obj(json!({ "env": { "type": "string" } }), &[]),
        output: || {
            obj(
                json!({ "agents": { "type": "array", "items": { "type": "object" } } }),
                &["agents"],
            )
        },
    },
    SessionFunction {
        name: "parrot.envs.list",
        methods: &["env/list"],
        description: "List the environments.",
        effects: &["read"],
        needs: Needs::Scope("read"),
        input: || obj(json!({}), &[]),
        output: || {
            obj(
                json!({ "envs": { "type": "array", "items": { "type": "object" } } }),
                &["envs"],
            )
        },
    },
];

/// Copies the named fields of `input` that are present into `params`.
fn copy(params: &mut Value, input: &Value, fields: &[&str]) {
    for f in fields {
        if let Some(v) = input.get(*f).filter(|v| !v.is_null()) {
            params[*f] = v.clone();
        }
    }
}

fn need_str<'a>(input: &'a Value, field: &str) -> Result<&'a str, Value> {
    input
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| rules::invalid_input(format!("{field} is required and must be a string")))
}

/// Calls a session function and returns the MCP tool result.
pub async fn call(sf: &SessionFunction, cx: &Context, input: Value) -> Value {
    match project(sf, cx, input).await {
        Ok(v) | Err(v) => v,
    }
}

async fn rpc(cx: &Context, method: &str, params: Value) -> Result<Value, Value> {
    match tokio::time::timeout(cx.limit, cx.upstream.call(method, params)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(rules::from_rpc(&e)),
        Err(_) => Err(rules::error(
            "timeout",
            format!("{method} did not answer in time"),
            Map::new(),
        )),
    }
}

async fn project(sf: &SessionFunction, cx: &Context, input: Value) -> Result<Value, Value> {
    if !input.is_object() {
        return Err(rules::invalid_input("the input must be an object"));
    }
    let me = cx.session.as_str();
    let mut params = json!({});
    if sf.keyed() {
        copy(&mut params, &input, &["key"]);
    }
    let output = match sf.name {
        "parrot.sessions.spawn" => {
            need_str(&input, "agent")?;
            need_str(&input, "brief")?;
            copy(&mut params, &input, &["agent", "env", "workspace", "onParentEnd"]);
            params["parent"] = json!(me);
            if params.get("workspace").is_none() {
                let Some(ws) = &cx.workspace else {
                    return Err(rules::invalid_input(
                        "workspace is required: the caller has none to share",
                    ));
                };
                params["workspace"] = json!({ "kind": "use", "workspace": ws });
            }
            let mut task = json!({});
            copy(&mut task, &input, &["brief", "title", "result", "completeOn"]);
            params["task"] = task;
            if let Some(on) = input.get("wake").filter(|v| v.is_array()) {
                params["wake"] = json!({ "on": on });
            }
            let r = rpc(cx, "session/new", params).await?;
            let mut out = json!({ "session": r["session"]["id"] });
            if let Some(task) = r["session"].get("task").filter(|t| t.is_object()) {
                out["task"] = task.clone();
            }
            out
        }
        "parrot.sessions.send" => {
            need_str(&input, "session")?;
            need_str(&input, "text")?;
            copy(&mut params, &input, &["session", "text", "delivery"]);
            rpc(cx, "session/prompt", params).await?
        }
        "parrot.sessions.act" => {
            need_str(&input, "session")?;
            need_str(&input, "action")?;
            copy(&mut params, &input, &["session", "action", "input"]);
            rpc(cx, "action/invoke", params).await?
        }
        "parrot.sessions.cancel" | "parrot.sessions.stop" => {
            need_str(&input, "session")?;
            copy(&mut params, &input, &["session", "tree"]);
            let method = if sf.name.ends_with("cancel") {
                "session/cancel"
            } else {
                "session/stop"
            };
            rpc(cx, method, params).await?
        }
        "parrot.sessions.read" => return read(cx, &input).await.map(rules::ok),
        "parrot.sessions.tree" => {
            params["session"] = json!(me);
            copy(&mut params, &input, &["cursor", "limit"]);
            rpc(cx, "session/tree", params).await?
        }
        "parrot.sessions.handoff" => {
            params["session"] = json!(me);
            copy(&mut params, &input, &["agent", "env"]);
            rpc(cx, "session/rebind", params).await?
        }
        "parrot.tasks.assign" => {
            need_str(&input, "session")?;
            need_str(&input, "brief")?;
            copy(&mut params, &input, &["session"]);
            let mut task = json!({});
            copy(&mut task, &input, &["brief", "title", "result", "completeOn"]);
            params["task"] = task;
            rpc(cx, "task/assign", params).await?
        }
        "parrot.tasks.wait" => return wait(cx, &input).await.map(rules::ok),
        "parrot.task.finish" => {
            let Some(output) = input.get("output") else {
                return Err(rules::invalid_input("output is required"));
            };
            rpc(cx, "task/finish", json!({ "session": me, "output": output })).await?
        }
        "parrot.task.fail" => {
            let error = need_str(&input, "error")?;
            rpc(cx, "task/fail", json!({ "session": me, "error": error })).await?
        }
        "parrot.requests.list" => {
            params["tree"] = json!(me);
            if input.get("status").and_then(Value::as_str) != Some("any") {
                params["status"] = json!("open");
            }
            copy(&mut params, &input, &["cursor"]);
            rpc(cx, "request/list", params).await?
        }
        "parrot.workspaces.status" => {
            let ws = input
                .get("workspace")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(cx.workspace.clone());
            let Some(ws) = ws else {
                return Err(rules::invalid_input("workspace is required"));
            };
            rpc(cx, "workspace/status", json!({ "workspace": ws })).await?
        }
        "parrot.workspaces.merge" => {
            need_str(&input, "from")?;
            copy(&mut params, &input, &["from", "into", "message"]);
            if params.get("into").is_none() {
                let Some(ws) = &cx.workspace else {
                    return Err(rules::invalid_input("into is required"));
                };
                params["into"] = json!(ws);
            }
            params["method"] = input.get("method").cloned().unwrap_or(json!("merge"));
            rpc(cx, "workspace/merge", params).await?
        }
        "parrot.agents.list" => {
            copy(&mut params, &input, &["env"]);
            rpc(cx, "agent/list", params).await?
        }
        "parrot.envs.list" => rpc(cx, "env/list", json!({})).await?,
        other => {
            return Err(rules::error(
                "unsupported",
                format!("{other} has no projection"),
                Map::new(),
            ));
        }
    };
    Ok(rules::ok(output))
}

async fn read(cx: &Context, input: &Value) -> Result<Value, Value> {
    let sessions = rules::strings(input, "sessions")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| vec![cx.session.clone()]);
    let cursor = input.get("cursor").and_then(Value::as_str);
    if cursor.is_some() && sessions.len() > 1 {
        return Err(rules::invalid_input("cursor needs exactly one session"));
    }
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(20).min(200);
    let fields = rules::strings(input, "fields");
    let mut entries = Vec::new();
    for id in &sessions {
        let entry = match rpc(cx, "session/get", json!({ "session": id })).await {
            Err(e) => json!({ "error": e["structuredContent"]["error"] }),
            Ok(got) => {
                let mut session = got["session"].clone();
                rules::select_fields(&mut session, fields.as_deref());
                let mut entry = json!({ "session": session, "items": [] });
                if limit > 0 {
                    let mut p = json!({ "session": id, "limit": limit, "direction": "back" });
                    if let Some(c) = cursor {
                        p["cursor"] = json!(c);
                    }
                    match rpc(cx, "session/read", p).await {
                        Ok(r) => {
                            entry["items"] = r["items"].clone();
                            if let Some(b) = r.get("before").filter(|b| !b.is_null()) {
                                entry["before"] = b.clone();
                            }
                        }
                        Err(e) => entry["error"] = e["structuredContent"]["error"].clone(),
                    }
                }
                entry
            }
        };
        entries.push(entry);
    }
    let kept = rules::cut(&mut entries, rules::MAX_RESULT);
    let mut results = Map::new();
    for (id, entry) in sessions.iter().zip(entries) {
        results.insert(id.clone(), entry);
    }
    let mut out = json!({ "results": results });
    if kept < sessions.len() {
        out["next"] = json!(sessions[kept..]);
    }
    Ok(out)
}

/// `task/wait` bounded by the call limit: the runtime is asked to return a little before the limit,
/// and if it still hasn't, the call returns as timed out with the cursor it was given.
async fn wait(cx: &Context, input: &Value) -> Result<Value, Value> {
    let Some(sessions) = rules::strings(input, "sessions") else {
        return Err(rules::invalid_input("sessions is required"));
    };
    let cap = cx
        .limit
        .saturating_sub(Duration::from_secs(2))
        .max(Duration::from_secs(1))
        .as_secs();
    let timeout = input
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(cap)
        .min(cap);
    let mut params = json!({ "sessions": sessions, "timeout": timeout });
    if let Some(c) = input.get("cursor").filter(|c| !c.is_null()) {
        params["since"] = c.clone();
    }
    match tokio::time::timeout(cx.limit, cx.upstream.call("task/wait", params)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(rules::from_rpc(&e)),
        Err(_) => Ok(
            json!({ "changes": [], "cursor": input.get("cursor").cloned().unwrap_or(Value::Null), "timedOut": true }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::check_name;

    #[test]
    fn names_follow_27_1() {
        for sf in ALL {
            check_name(sf.name).unwrap();
        }
    }

    #[test]
    fn schemas_are_objects_with_outputs() {
        for sf in ALL {
            assert_eq!(sf.input_schema()["type"], "object", "{}", sf.name);
            assert_eq!((sf.output)()["type"], "object", "{}", sf.name);
            assert_eq!(
                sf.input_schema()["properties"].get("key").is_some(),
                sf.keyed(),
                "{}",
                sf.name
            );
        }
    }
}
