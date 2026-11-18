//! Options and permission levels (spec 15, Appendix E8 `option/list`).

use std::collections::BTreeMap;

use crate::requests::{allows, forbidden, invalid, wrong_state};
use crate::{CallOwner, Cx, Reply, directory};
use parrot_proto::driver::{BindingOptionsParams, BindingOptionsResult, DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::option::{
    Applies, ConfigOption, OptionCompleteParams, OptionListParams, OptionListResult, OptionSetParams,
    OptionSetResult, PermissionLevel,
};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Map, Value, json};

/// Options by session.
#[derive(Debug, Default)]
pub struct Options {
    pub by_session: BTreeMap<String, SessionOptions>,
    /// Outstanding driver calls by call id.
    pub calls: BTreeMap<u64, PendingCall>,
}

/// A session's option set as the driver last reported it.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    pub options: Vec<ConfigOption>,
    pub level: Option<PermissionLevel>,
    pub network: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct PendingCall {
    pub session: String,
    pub reply_to: Option<(String, RequestId)>,
    pub kind: PendingKind,
}

#[derive(Debug, Clone)]
pub enum PendingKind {
    Set { reset: Vec<String> },
    Complete,
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "option/list" => Reply::Now(list(cx, conn, params)),
        "option/set" => set(cx, conn, id, params),
        "option/complete" => complete(cx, conn, id, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn list(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    if !allows(cx.state, conn, "read") {
        return Err(forbidden("read"));
    }
    let p: OptionListParams = serde_json::from_value(params).map_err(|e| invalid(e.to_string()))?;
    if !cx.state.sessions.by_id.contains_key(&p.session) {
        return Err(RpcError::new(-32002, format!("no session {}", p.session)));
    }
    let o = cx
        .state
        .options
        .by_session
        .get(&p.session)
        .cloned()
        .unwrap_or_default();
    Ok(serde_json::to_value(OptionListResult {
        options: o.options,
        level: o.level,
        network: o.network,
    })
    .unwrap_or_default())
}

fn set(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    match prepare_set(cx, conn, params) {
        Err(e) => Reply::Now(Err(e)),
        Ok((session, binding, values, reset)) => {
            let params = BindingOptionsParams {
                binding: binding.clone(),
                values,
            };
            let call = cx.call_driver(
                CallOwner::Options,
                &binding,
                "binding/options",
                serde_json::to_value(params).unwrap_or_default(),
            );
            let reply_to = id.map(|id| (conn.to_string(), id.clone()));
            let later = reply_to.is_some();
            cx.state.options.calls.insert(
                call,
                PendingCall {
                    session,
                    reply_to,
                    kind: PendingKind::Set { reset },
                },
            );
            if later {
                Reply::Later
            } else {
                Reply::Now(Ok(json!({})))
            }
        }
    }
}

type Prepared = (String, String, Map<String, Value>, Vec<String>);

/// Check `option/set` and work out the values to send, with dependent values reset.
fn prepare_set(cx: &mut Cx, conn: &str, params: Value) -> Result<Prepared, RpcError> {
    if !allows(cx.state, conn, "configure") {
        return Err(forbidden("configure"));
    }
    let p: OptionSetParams = serde_json::from_value(params).map_err(|e| invalid(e.to_string()))?;
    if !cx.state.sessions.by_id.contains_key(&p.session) {
        return Err(RpcError::new(-32002, format!("no session {}", p.session)));
    }
    let Some(binding) = directory::binding_of(cx.state, &p.session) else {
        return Err(wrong_state("stopped", "the session has no running binding"));
    };
    let options = cx
        .state
        .options
        .by_session
        .get(&p.session)
        .map(|o| o.options.clone())
        .unwrap_or_default();
    let find = |id: &str| options.iter().find(|o| o.id == id);

    // The values after the change, to pick each dependent option's schema.
    let mut after: Map<String, Value> = options.iter().map(|o| (o.id.clone(), o.value.clone())).collect();
    for (k, v) in &p.values {
        after.insert(k.clone(), v.clone());
    }
    let schema_of = |o: &ConfigOption| -> Value {
        o.depends_on
            .as_ref()
            .and_then(|d| {
                let parent = after.get(&d.option)?;
                let key = match parent {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                d.schemas.get(&key).cloned()
            })
            .unwrap_or_else(|| o.schema.clone())
    };

    for (k, v) in &p.values {
        let Some(o) = find(k) else {
            return Err(invalid(format!("unknown option {k}")).with_data(json!({ "option": k })));
        };
        if o.applies == Applies::Start {
            return Err(wrong_state("running", format!("{k} applies only at start")));
        }
        if let Err(why) = validate(&schema_of(o), v) {
            return Err(invalid(format!("{k}: {why}")).with_data(json!({ "option": k })));
        }
    }
    let mut values = p.values.clone();
    let mut reset = Vec::new();
    for o in &options {
        if values.contains_key(&o.id)
            || o.depends_on
                .as_ref()
                .is_none_or(|d| !p.values.contains_key(&d.option))
        {
            continue;
        }
        let schema = schema_of(o);
        if validate(&schema, &o.value).is_err() {
            let default = schema
                .get("default")
                .cloned()
                .or_else(|| o.default.clone())
                .unwrap_or(Value::Null);
            values.insert(o.id.clone(), default);
            reset.push(o.id.clone());
        }
    }
    Ok((p.session, binding, values, reset))
}

fn complete(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    if !allows(cx.state, conn, "configure") {
        return Reply::Now(Err(forbidden("configure")));
    }
    let p: OptionCompleteParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return Reply::Now(Err(invalid(e.to_string()))),
    };
    let unsupported = || {
        RpcError::new(-32001, "the option has no completion")
            .with_data(json!({ "feature": "option.complete" }))
    };
    let Some(session) = p.session.clone() else {
        return Reply::Now(Err(unsupported()));
    };
    let has_source = cx
        .state
        .options
        .by_session
        .get(&session)
        .and_then(|o| o.options.iter().find(|o| o.id == p.option))
        .is_some_and(|o| o.complete == Some(true));
    let binding = directory::binding_of(cx.state, &session);
    let (true, Some(binding)) = (has_source, binding) else {
        return Reply::Now(Err(unsupported()));
    };
    let params = json!({ "binding": binding, "option": p.option, "prefix": p.prefix, "limit": p.limit });
    let call = cx.call_driver(CallOwner::Options, &binding, "option/complete", params);
    let reply_to = id.map(|id| (conn.to_string(), id.clone()));
    let later = reply_to.is_some();
    cx.state.options.calls.insert(
        call,
        PendingCall {
            session,
            reply_to,
            kind: PendingKind::Complete,
        },
    );
    if later {
        Reply::Later
    } else {
        Reply::Now(Ok(json!({ "values": [] })))
    }
}

/// A driver answered a call this module made with `Cx::call_driver`.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(pending) = cx.state.options.calls.remove(&call) else {
        return;
    };
    let result = match (pending.kind, result) {
        (_, Err(e)) => Err(e),
        (PendingKind::Complete, Ok(v)) => Ok(v),
        (PendingKind::Set { reset }, Ok(v)) => match serde_json::from_value::<BindingOptionsResult>(v) {
            Err(e) => Err(RpcError::new(-32020, "bad binding/options result")
                .with_data(json!({ "detail": e.to_string() }))),
            Ok(r) => {
                store(cx, &pending.session, r.options.clone(), r.level, r.network);
                Ok(serde_json::to_value(OptionSetResult {
                    options: r.options,
                    reset,
                    level: Some(r.level),
                })
                .unwrap_or_default())
            }
        },
    };
    if let Some((conn, id)) = pending.reply_to {
        cx.reply(&conn, id, result);
    }
}

fn store(cx: &mut Cx, session: &str, options: Vec<ConfigOption>, level: PermissionLevel, network: bool) {
    cx.state.options.by_session.insert(
        session.to_string(),
        SessionOptions {
            options: options.clone(),
            level: Some(level),
            network: Some(network),
        },
    );
    cx.publish(
        Some(session.to_string()),
        json!({ "type": "options", "options": options, "level": level, "network": network }),
    );
}

/// A driver update of this area.
pub fn on_update(cx: &mut Cx, update: &DriverUpdate) {
    let Open::Known(DriverUpdateKind::Options {
        options,
        level,
        network,
    }) = &update.update
    else {
        return;
    };
    let Some(session) = directory::session_of(cx.state, &update.binding) else {
        return;
    };
    store(cx, &session, options.clone(), *level, *network);
}

/// Check a value against the parts of JSON Schema that options use.
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    let Some(s) = schema.as_object() else {
        return Ok(());
    };
    if let Some(c) = s.get("const")
        && c != value
    {
        return Err(format!("must be {c}"));
    }
    if let Some(Value::Array(e)) = s.get("enum")
        && !e.contains(value)
    {
        return Err("not one of the allowed values".into());
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(branches)) = s.get(key)
            && !branches.iter().any(|b| validate(b, value).is_ok())
        {
            return Err("not one of the choices".into());
        }
    }
    if let Some(t) = s.get("type") {
        let types: Vec<&str> = match t {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let ok = types.is_empty()
            || types.iter().any(|t| match *t {
                "string" => value.is_string(),
                "number" => value.is_number(),
                "integer" => value.is_i64() || value.is_u64(),
                "boolean" => value.is_boolean(),
                "array" => value.is_array(),
                "object" => value.is_object(),
                "null" => value.is_null(),
                _ => true,
            });
        if !ok {
            return Err(format!("must be of type {}", types.join(" or ")));
        }
    }
    if let Some(n) = value.as_f64() {
        if s.get("minimum").and_then(Value::as_f64).is_some_and(|m| n < m) {
            return Err("below the minimum".into());
        }
        if s.get("maximum").and_then(Value::as_f64).is_some_and(|m| n > m) {
            return Err("above the maximum".into());
        }
    }
    if let Some(text) = value.as_str() {
        let len = text.chars().count() as u64;
        if s.get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len < m)
        {
            return Err("too short".into());
        }
        if s.get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len > m)
        {
            return Err("too long".into());
        }
    }
    Ok(())
}
