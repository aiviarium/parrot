//! Requests (spec 14, 21, Appendix E4 and E8): what drivers ask, and who answers.

use std::collections::BTreeMap;

use crate::{CallOwner, Cx, Reply, State, directory};
use parrot_proto::driver::{BindingAnswerParams, DriverUpdate, DriverUpdateKind};
use parrot_proto::open::Open;
use parrot_proto::option::PermissionLevel;
use parrot_proto::request::{
    Answer, Answerable, ChoiceIntent, ChoiceScope, Request, RequestAnswerParams, RequestListParams,
    RequestListResult, RequestStatus,
};
use parrot_proto::rpc::{RequestId, RpcError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Requests by Parrot id.
#[derive(Debug, Default)]
pub struct Requests {
    pub by_id: BTreeMap<String, RequestRecord>,
    /// Outstanding `binding/answer` calls by call id, to the request they answer.
    pub calls: BTreeMap<u64, String>,
    /// The connection whose requests a session raises, for `answerable: "connection"`.
    pub session_conn: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RequestRecord {
    pub request: Request,
    pub binding: String,
    /// The driver key of the request.
    pub key: String,
    /// The connection a `connection` request belongs to.
    pub conn: Option<String>,
    /// When `request.expires` passes, in ms since the epoch.
    pub expires_ms: Option<u64>,
    pub pending: Option<PendingAnswer>,
    /// The key the winning answer came with, so a repeat with it succeeds.
    pub answer_key: Option<String>,
}

/// An answer sent to the driver but not confirmed yet.
#[derive(Debug, Clone)]
pub struct PendingAnswer {
    /// The client call to reply to; `None` for runtime answers (fallback, ceiling).
    pub reply_to: Option<(String, RequestId)>,
    pub answer: Answer,
    pub by: String,
    pub key: Option<String>,
}

/// Whether `conn` holds the scope named `scope`; unknown names are never held.
pub(crate) fn allows(state: &State, conn: &str, scope: &str) -> bool {
    serde_json::from_value::<parrot_proto::principal::Scope>(serde_json::Value::String(scope.to_string()))
        .is_ok_and(|scope| crate::principals::allows(state, conn, scope))
}

use crate::principals::principal_of;

/// The level of the ceiling over a session, if any (spec 22).
pub fn ceiling_level(state: &State, session: &str) -> Option<PermissionLevel> {
    crate::ceilings::of(state, session).level
}

/// Answer a request on the runtime's behalf (`answeredBy` "ceiling" or "fallback").
pub fn answer_as_runtime(cx: &mut Cx, request: &str, answer: Answer, by: &str) {
    send_answer(cx, request, answer, by.to_string(), None, None);
}

/// The Parrot id for a driver key (spec 30.4).
pub fn derive_id(session: &str, binding: &str, kind: &str, key: &str) -> String {
    let mut h = Sha256::new();
    for part in [session, binding, kind, key] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("req_{}", &hex[..26])
}

/// Persist and publish a request as it now stands.
fn publish_request(cx: &mut Cx, id: &str) {
    let Some(rec) = cx.state.requests.by_id.get(id) else {
        return;
    };
    let session = rec.request.session.clone();
    let stored = json!({ "request": rec.request, "binding": rec.binding, "key": rec.key });
    let update = json!({ "type": "request", "request": rec.request });
    cx.persist(json!({ "op": "put", "kind": "request", "id": id, "value": stored }));
    cx.publish(Some(session), update);
}

/// Put back a request read from the store. After a restart the harness that raised it is gone:
/// `connection` requests expire, `message` requests stay open (spec 34, Appendix E8).
pub fn restore(cx: &mut Cx, id: &str, value: &Value) {
    let Some(request) = value
        .get("request")
        .and_then(|r| serde_json::from_value::<Request>(r.clone()).ok())
    else {
        return;
    };
    let str_of = |k: &str| {
        value
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let open = request.status == RequestStatus::Open;
    let expires_ms = request.expires.as_deref().and_then(parse_rfc3339_ms);
    let connection = request.answerable == Answerable::Connection;
    cx.state.requests.by_id.insert(
        id.to_string(),
        RequestRecord {
            request,
            binding: str_of("binding"),
            key: str_of("key"),
            conn: None,
            expires_ms,
            pending: None,
            answer_key: None,
        },
    );
    if open && connection {
        if let Some(rec) = cx.state.requests.by_id.get_mut(id) {
            rec.request.status = RequestStatus::Expired;
        }
        publish_request(cx, id);
    } else if open && let Some(at_ms) = expires_ms {
        cx.emit(crate::Effect::TimerAt { at_ms });
    }
}

/// The open requests of a session and its descendants (spec 10.2).
pub fn open_in(state: &State, session: &str) -> Vec<Request> {
    state
        .requests
        .by_id
        .values()
        .map(|r| &r.request)
        .filter(|r| r.status == RequestStatus::Open && r.path.iter().any(|p| p == session))
        .cloned()
        .collect()
}

/// Tie the `connection` requests a session raises from now on to `conn`.
pub fn note_connection(state: &mut State, session: &str, conn: &str) {
    state
        .requests
        .session_conn
        .insert(session.to_string(), conn.to_string());
}

pub(crate) fn forbidden(scope: &str) -> RpcError {
    RpcError::new(-32003, format!("needs scope {scope}"))
        .with_data(json!({ "reason": "scope", "scope": scope }))
}

pub(crate) fn invalid(msg: impl Into<String>) -> RpcError {
    RpcError::new(-32602, msg)
}

pub(crate) fn wrong_state(state: &str, msg: impl Into<String>) -> RpcError {
    RpcError::new(-32004, msg).with_data(json!({ "state": state }))
}

/// Handle a client method of this area.
pub fn call(cx: &mut Cx, conn: &str, id: Option<&RequestId>, method: &str, params: Value) -> Reply {
    match method {
        "request/list" => Reply::Now(list(cx, conn, params)),
        "request/answer" => answer(cx, conn, id, params),
        _ => Reply::Now(Err(RpcError::new(-32601, format!("method not found: {method}")))),
    }
}

fn list(cx: &mut Cx, conn: &str, params: Value) -> Result<Value, RpcError> {
    if !allows(cx.state, conn, "read") {
        return Err(forbidden("read"));
    }
    let params = if params.is_null() { json!({}) } else { params };
    let p: RequestListParams = serde_json::from_value(params).map_err(|e| invalid(e.to_string()))?;
    let requests = cx
        .state
        .requests
        .by_id
        .values()
        .map(|r| &r.request)
        .filter(|r| p.session.as_ref().is_none_or(|s| &r.session == s))
        .filter(|r| p.tree.as_ref().is_none_or(|t| r.path.contains(t)))
        .filter(|r| p.status.is_none_or(|s| r.status == s))
        .cloned()
        .collect();
    Ok(serde_json::to_value(RequestListResult { requests, next: None }).unwrap_or_default())
}

fn answer(cx: &mut Cx, conn: &str, id: Option<&RequestId>, params: Value) -> Reply {
    if !allows(cx.state, conn, "answer") {
        return Reply::Now(Err(forbidden("answer")));
    }
    let p: RequestAnswerParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return Reply::Now(Err(invalid(e.to_string()))),
    };
    let Some(rec) = cx.state.requests.by_id.get(&p.request) else {
        return Reply::Now(Err(RpcError::new(-32002, format!("no request {}", p.request))));
    };
    let same_key = |k: &Option<String>| p.key.is_some() && *k == p.key;
    if rec.request.status != RequestStatus::Open || rec.pending.is_some() {
        if same_key(&rec.answer_key) || rec.pending.as_ref().is_some_and(|pa| same_key(&pa.key)) {
            return Reply::Now(Ok(json!({})));
        }
        return Reply::Now(Err(RpcError::new(-32013, "already answered")));
    }
    if let (Some(choice), Some(choices)) = (&p.answer.choice, &rec.request.choices)
        && !choices.iter().any(|c| &c.id == choice)
    {
        return Reply::Now(Err(invalid(format!("unknown choice {choice}"))));
    }
    let by = principal_of(cx.state, conn);
    let live = cx.state.bindings.info.get(&rec.binding).is_some_and(|b| {
        matches!(
            b.status,
            parrot_proto::session::BindingStatus::Starting | parrot_proto::session::BindingStatus::Running
        )
    });
    if rec.request.answerable == Answerable::Message && !live {
        answer_by_message(cx, &p.request, p.answer, by, p.key);
        return Reply::Now(Ok(json!({})));
    }
    let reply_to = id.map(|id| (conn.to_string(), id.clone()));
    let later = reply_to.is_some();
    send_answer(cx, &p.request, p.answer, by, p.key, reply_to);
    if later {
        Reply::Later
    } else {
        Reply::Now(Ok(json!({})))
    }
}

/// Answer a `message` request whose harness is gone: the answer becomes the next prompt (spec 14,
/// Appendix E4).
fn answer_by_message(cx: &mut Cx, request: &str, answer: Answer, by: String, key: Option<String>) {
    let Some(rec) = cx.state.requests.by_id.get_mut(request) else {
        return;
    };
    let mut text = format!("Answer to \"{}\":", rec.request.title);
    if let Some(c) = &answer.choice {
        let label = rec
            .request
            .choices
            .as_ref()
            .and_then(|cs| cs.iter().find(|x| &x.id == c))
            .map_or(c.as_str(), |x| x.label.as_str());
        text.push_str(&format!(" {label}."));
    }
    for v in [&answer.value, &answer.input].into_iter().flatten() {
        text.push_str(&format!("\n{v}"));
    }
    if let Some(m) = &answer.message {
        text.push_str(&format!("\n{m}"));
    }
    rec.request.status = RequestStatus::Answered;
    rec.request.answer = Some(answer);
    rec.request.answered_by = Some(by.clone());
    rec.answer_key = key;
    let session = rec.request.session.clone();
    publish_request(cx, request);
    let origin = json!({ "kind": "runtime", "principal": by });
    crate::queue::enqueue(
        cx,
        &session,
        parrot_proto::prompt::QueueEntryKind::Prompt,
        text,
        origin,
        json!({}),
    );
    crate::queue::dispatch(cx, &session);
}

/// Send an answer to the driver; the request resolves when the driver confirms.
fn send_answer(
    cx: &mut Cx,
    request: &str,
    answer: Answer,
    by: String,
    key: Option<String>,
    reply_to: Option<(String, RequestId)>,
) {
    let Some(rec) = cx.state.requests.by_id.get_mut(request) else {
        return;
    };
    let params = BindingAnswerParams {
        binding: rec.binding.clone(),
        request: rec.key.clone(),
        answer: answer.clone(),
    };
    rec.pending = Some(PendingAnswer {
        reply_to,
        answer,
        by,
        key,
    });
    let binding = rec.binding.clone();
    let call = cx.call_driver(
        CallOwner::Requests,
        &binding,
        "binding/answer",
        serde_json::to_value(params).unwrap_or_default(),
    );
    cx.state.requests.calls.insert(call, request.to_string());
}

/// A driver answered a call this module made with `Cx::call_driver`.
pub fn on_reply(cx: &mut Cx, call: u64, result: Result<Value, RpcError>) {
    let Some(rid) = cx.state.requests.calls.remove(&call) else {
        return;
    };
    let Some(rec) = cx.state.requests.by_id.get_mut(&rid) else {
        return;
    };
    let Some(pending) = rec.pending.take() else {
        return;
    };
    match result {
        Ok(_) => {
            if rec.request.status == RequestStatus::Open {
                rec.request.status = RequestStatus::Answered;
                rec.request.answer = Some(pending.answer);
                rec.request.answered_by = Some(pending.by);
                rec.answer_key = pending.key;
            }
            publish_request(cx, &rid);
            if let Some((conn, id)) = pending.reply_to {
                cx.reply(&conn, id, Ok(json!({})));
            }
        }
        Err(e) => {
            if let Some((conn, id)) = pending.reply_to {
                cx.reply(&conn, id, Err(e));
            }
        }
    }
}

/// A driver update of this area.
pub fn on_update(cx: &mut Cx, update: &DriverUpdate) {
    let Open::Known(DriverUpdateKind::Request { request }) = &update.update else {
        return;
    };
    let Some(session) = directory::session_of(cx.state, &update.binding) else {
        return;
    };
    let key = request.id.clone();
    let id = derive_id(&session, &update.binding, "request", &key);
    let mut req = (**request).clone();
    req.id = id.clone();
    req.session = session.clone();
    req.path = directory::path_of(cx.state, &session);
    // The turn and item are driver keys; clients know them by their runtime ids.
    if let Some(t) = &req.turn {
        req.turn = Some(crate::bindings::map_key(cx, &update.binding, "trn", t, None));
    }
    if let Some(i) = &req.item {
        req.item = Some(crate::bindings::map_key(cx, &update.binding, "itm", i, None));
    }

    if let Some(rec) = cx.state.requests.by_id.get_mut(&id) {
        // A later report of a known request: the harness may have settled it itself.
        if rec.request.status != RequestStatus::Open {
            return;
        }
        if req.status == RequestStatus::Answered && req.answered_by.is_none() {
            req.answered_by = Some("harness".into());
        }
        rec.expires_ms = req.expires.as_deref().and_then(parse_rfc3339_ms);
        rec.request = req;
        publish_request(cx, &id);
        return;
    }

    let ceiling = ceiling_level(cx.state, &session);
    if ceiling.is_some()
        && let Some(choices) = &mut req.choices
    {
        // An answer under a ceiling can't raise the session's level for later.
        choices.retain(|c| c.scope.is_none_or(|s| s == ChoiceScope::Once));
    }
    let conn = match req.answerable {
        Answerable::Connection => cx.state.requests.session_conn.get(&session).cloned(),
        Answerable::Message => None,
    };
    let expires_ms = req.expires.as_deref().and_then(parse_rfc3339_ms);
    if let Some(at_ms) = expires_ms {
        cx.emit(crate::Effect::TimerAt { at_ms });
    }
    let above = ceiling.is_some_and(|c| req.level > c);
    let deny = req
        .choices
        .as_ref()
        .and_then(|cs| cs.iter().find(|c| c.intent == Some(ChoiceIntent::Deny)))
        .map(|c| c.id.clone());
    cx.state.requests.by_id.insert(
        id.clone(),
        RequestRecord {
            request: req.clone(),
            binding: update.binding.clone(),
            key,
            conn,
            expires_ms,
            pending: None,
            answer_key: None,
        },
    );
    publish_request(cx, &id);
    if !above {
        crate::ceilings::on_request(cx, &id, &session);
        crate::tasks::on_request(cx, &session);
    }
    if above {
        let answer = Answer {
            choice: deny,
            message: Some("denied by the session's ceiling".into()),
            ..Answer::default()
        };
        send_answer(cx, &id, answer, "ceiling".into(), None, None);
    }
}

/// Settle an open request that nobody may answer any more: its fallback, else `expired`.
fn expire(cx: &mut Cx, id: &str) {
    let Some(rec) = cx.state.requests.by_id.get_mut(id) else {
        return;
    };
    if rec.request.status != RequestStatus::Open || rec.pending.is_some() {
        return;
    }
    if let Some(fallback) = rec.request.fallback.clone() {
        send_answer(cx, id, fallback, "fallback".into(), None, None);
        return;
    }
    rec.request.status = RequestStatus::Expired;
    publish_request(cx, id);
}

/// Time passed: expire requests.
pub fn tick(cx: &mut Cx) {
    let now = cx.now_ms;
    let due: Vec<String> = cx
        .state
        .requests
        .by_id
        .iter()
        .filter(|(_, r)| r.request.status == RequestStatus::Open && r.expires_ms.is_some_and(|e| e <= now))
        .map(|(id, _)| id.clone())
        .collect();
    for id in due {
        expire(cx, &id);
    }
}

/// A connection closed: expire its `connection` requests.
pub fn disconnected(cx: &mut Cx, conn: &str) {
    // Requests survive a client leaving (spec 14): another client, or the same one after
    // reconnecting, can still answer. Only the harness side ends them.
    cx.state.requests.session_conn.retain(|_, c| c != conn);
}

/// A binding's harness or driver went away: its `connection` requests can't be answered.
pub fn binding_gone(cx: &mut Cx, binding: &str) {
    let gone: Vec<String> = cx
        .state
        .requests
        .by_id
        .iter()
        .filter(|(_, r)| r.binding == binding && r.request.answerable == Answerable::Connection)
        .map(|(id, _)| id.clone())
        .collect();
    for id in gone {
        if let Some(rec) = cx.state.requests.by_id.get_mut(&id)
            && rec.request.status == RequestStatus::Open
        {
            rec.request.status = RequestStatus::Expired;
            publish_request(cx, &id);
        }
    }
}

/// RFC 3339 time to ms since the epoch; `None` when it doesn't parse.
pub fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        i = start;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let digits: String = s.get(start..i)?.chars().chain("000".chars()).take(3).collect();
        ms = digits.parse().ok()?;
    }
    let offset_min = match b.get(i)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let off = num(i + 1..i + 3)? * 60 + num(i + 4..i + 6)?;
            if *sign == b'+' { off } else { -off }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    u64::try_from(secs * 1000 + ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(parse_rfc3339_ms("1970-01-02T00:00:00.5Z"), Some(86_400_500));
        assert_eq!(parse_rfc3339_ms("1970-01-01T02:00:01+02:00"), Some(1000));
        assert_eq!(parse_rfc3339_ms("nope"), None);
    }
}
