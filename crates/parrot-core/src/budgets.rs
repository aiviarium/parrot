//! Budgets as allotments (spec 17.2, Appendix E2, E9).
//!
//! A child is created with an allotment taken from its parent's remaining budget. Each session
//! spends from its own allotment; the unspent part goes back to the parent when the child's task
//! ends or the child is deleted. After that, what the child spends is charged to the parent.

use crate::{Cx, State};
use parrot_proto::rpc::RpcError;
use parrot_proto::usage::{Budget, Cost, Usage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A session's allotment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Allotment {
    pub limit: Budget,
    /// Spent by the session itself and by children whose allotments came back.
    pub used: Budget,
    /// Held by children whose allotments haven't come back yet.
    pub allotted: Budget,
    /// The allotment went back to the parent.
    #[serde(default)]
    pub returned: bool,
    /// Exhaustion has been acted on.
    #[serde(default)]
    pub exhausted: bool,
}

/// The counted dimensions other than cost.
const DIMS: [&str; 4] = ["tokens", "seconds", "turns", "sessions"];

fn get(b: &Budget, dim: &str) -> Option<u64> {
    match dim {
        "tokens" => b.tokens,
        "seconds" => b.seconds,
        "turns" => b.turns,
        _ => b.sessions,
    }
}

fn slot<'a>(b: &'a mut Budget, dim: &str) -> &'a mut Option<u64> {
    match dim {
        "tokens" => &mut b.tokens,
        "seconds" => &mut b.seconds,
        "turns" => &mut b.turns,
        _ => &mut b.sessions,
    }
}

fn cost_of(b: &Budget) -> f64 {
    b.cost.as_ref().map_or(0.0, |c| c.amount)
}

/// `into += delta`, dimension by dimension.
pub fn add(into: &mut Budget, delta: &Budget) {
    for d in DIMS {
        if let Some(v) = get(delta, d) {
            *slot(into, d) = Some(slot(into, d).unwrap_or(0) + v);
        }
    }
    if let Some(c) = &delta.cost {
        let cur = into.cost.get_or_insert_with(|| Cost {
            amount: 0.0,
            currency: c.currency.clone(),
        });
        if cur.currency == c.currency {
            cur.amount += c.amount;
        }
    }
}

/// `into -= delta`, never below zero.
pub fn sub(into: &mut Budget, delta: &Budget) {
    for d in DIMS {
        if let (Some(cur), Some(v)) = (get(into, d), get(delta, d)) {
            *slot(into, d) = Some(cur.saturating_sub(v));
        }
    }
    if let (Some(cur), Some(c)) = (&mut into.cost, &delta.cost) {
        cur.amount = (cur.amount - c.amount).max(0.0);
    }
}

/// What is left of an allotment for the session and new children.
pub fn remaining(a: &Allotment) -> Budget {
    let mut r = Budget::default();
    for d in DIMS {
        if let Some(l) = get(&a.limit, d) {
            let spent = get(&a.used, d).unwrap_or(0) + get(&a.allotted, d).unwrap_or(0);
            *slot(&mut r, d) = Some(l.saturating_sub(spent));
        }
    }
    if let Some(c) = &a.limit.cost {
        r.cost = Some(Cost {
            amount: (c.amount - cost_of(&a.used) - cost_of(&a.allotted)).max(0.0),
            currency: c.currency.clone(),
        });
    }
    r
}

/// Whether a limited dimension other than `sessions` is spent: running out of sessions only
/// stops new children.
pub fn spent(a: &Allotment) -> bool {
    ["tokens", "seconds", "turns"].iter().any(|d| {
        let used = get(&a.used, d).unwrap_or(0);
        get(&a.limit, d).is_some_and(|l| used >= l && used > 0)
    }) || a
        .limit
        .cost
        .as_ref()
        .is_some_and(|c| cost_of(&a.used) >= c.amount && cost_of(&a.used) > 0.0)
}

/// The JSON of `Session.budget` (`BudgetState`).
pub fn json_of(a: &Allotment) -> Value {
    json!({ "limit": a.limit, "used": a.used, "allotted": a.allotted, "returned": a.returned })
}

/// Check and take a child's allotment from its parent (E9). `asked` is the child's `budget`.
pub fn allot(state: &mut State, parent: &str, asked: Option<&Budget>) -> Result<Option<Allotment>, RpcError> {
    let Some(pa) = state
        .tree
        .nodes
        .get(parent)
        .and_then(|n| n.budget.clone())
        .filter(|a| !a.returned)
    else {
        return Ok(asked.map(|b| Allotment {
            limit: b.clone(),
            ..Default::default()
        }));
    };
    let Some(asked) = asked else {
        return Err(RpcError::new(
            -32602,
            "the parent has a budget: session/new needs an explicit budget",
        )
        .with_data(json!({ "reason": "budget" })));
    };
    let left = remaining(&pa);
    let mut need = asked.clone();
    // Creating a child counts as one session of the parent's budget.
    add(
        &mut need,
        &Budget {
            sessions: Some(1),
            ..Default::default()
        },
    );
    for d in DIMS {
        if let Some(l) = get(&left, d) {
            if d != "sessions" && get(asked, d).is_none() {
                return Err(RpcError::new(
                    -32602,
                    format!("the budget must give {d}, which the parent limits"),
                )
                .with_data(json!({ "reason": "budget" })));
            }
            if get(&need, d).unwrap_or(0) > l {
                return Err(crate::ceilings::forbidden(
                    "budget",
                    format!("the parent has {l} {d} left"),
                ));
            }
        }
    }
    if let Some(c) = &left.cost {
        let want = asked.cost.as_ref().map(|c| c.amount);
        match want {
            None => {
                return Err(
                    RpcError::new(-32602, "the budget must give cost, which the parent limits")
                        .with_data(json!({ "reason": "budget" })),
                );
            }
            Some(w) if w > c.amount => {
                return Err(crate::ceilings::forbidden(
                    "budget",
                    "the parent's cost budget is too small",
                ));
            }
            _ => {}
        }
    }
    let node = state.tree.nodes.get_mut(parent).expect("checked above");
    let a = node.budget.as_mut().expect("checked above");
    add(
        &mut a.used,
        &Budget {
            sessions: Some(1),
            ..Default::default()
        },
    );
    add(&mut a.allotted, asked);
    Ok(Some(Allotment {
        limit: asked.clone(),
        ..Default::default()
    }))
}

/// The session whose allotment pays for what `session` spends: itself, or the nearest ancestor
/// with an allotment that hasn't gone back.
fn payer(state: &State, session: &str) -> Option<String> {
    let path = crate::directory::path_of(state, session);
    path.into_iter().rev().find(|s| {
        state
            .tree
            .nodes
            .get(s)
            .and_then(|n| n.budget.as_ref())
            .is_some_and(|a| !a.returned)
    })
}

/// Charge spending to a session; acts on exhaustion.
pub fn charge(cx: &mut Cx, session: &str, delta: &Budget) {
    let Some(payer) = payer(cx.state, session) else {
        return;
    };
    let Some(a) = cx
        .state
        .tree
        .nodes
        .get_mut(&payer)
        .and_then(|n| n.budget.as_mut())
    else {
        return;
    };
    add(&mut a.used, delta);
    let exhausted = spent(a) && !a.exhausted;
    if exhausted {
        a.exhausted = true;
    }
    crate::tree::persist(cx, &payer);
    crate::sessions::changed(cx, &payer);
    if exhausted {
        exhaust(cx, &payer);
    }
}

/// A usage report from a binding: tokens (input without cache reads, plus output) and cost.
pub fn on_usage(cx: &mut Cx, session: &str, usage: &Usage) {
    if usage.counted_in.is_some() {
        return;
    }
    let mut delta = Budget::default();
    if let Some(t) = &usage.tokens {
        delta.tokens = Some(t.input + t.output);
    }
    delta.cost = usage.cost.clone();
    charge(cx, session, &delta);
}

/// A turn started.
pub fn on_turn(cx: &mut Cx, session: &str) {
    charge(
        cx,
        session,
        &Budget {
            turns: Some(1),
            ..Default::default()
        },
    );
}

/// A budget ran out: interrupt running turns in the subtree, fail their tasks with `budget` and
/// pause their queues (spec 17.2, E8).
pub fn exhaust(cx: &mut Cx, root: &str) {
    for s in crate::tree::subtree(cx.state, root) {
        if let Some(q) = cx.state.queue.by_session.get_mut(&s) {
            q.paused = true;
        }
        let running = cx.state.sessions.info.get(&s).is_some_and(|i| {
            matches!(
                i.state,
                parrot_proto::state::State::Running | parrot_proto::state::State::RequiresAction
            )
        });
        if running
            && crate::sessions::active(cx.state, &s)
            && let Some(b) = crate::directory::binding_of(cx.state, &s)
        {
            cx.call_driver(
                crate::CallOwner::Tasks,
                &b,
                "binding/cancel",
                json!({ "binding": b }),
            );
        }
        let open = cx
            .state
            .tree
            .nodes
            .get(&s)
            .and_then(|n| n.task.as_ref())
            .is_some_and(|t| t.open());
        if open {
            crate::tasks::set_status(cx, &s, "failed", None, Some("budget".into()));
        }
        cx.publish(
            Some(s.clone()),
            json!({ "type": "queue", "queue": crate::queue::queue_state(cx.state, &s) }),
        );
        cx.publish(
            Some(s.clone()),
            json!({ "type": "notice", "level": "warning", "text": "the budget ran out", "code": "budget" }),
        );
    }
}

/// Give a child's unspent allotment back to its parent: when its task ends or it is deleted.
pub fn give_back(cx: &mut Cx, child: &str) {
    let parent = cx.state.sessions.by_id.get(child).and_then(|r| r.parent.clone());
    let Some(a) = cx.state.tree.nodes.get_mut(child).and_then(|n| n.budget.as_mut()) else {
        return;
    };
    if a.returned {
        return;
    }
    let Some(parent) = parent else {
        return;
    };
    let has_parent_budget = cx
        .state
        .tree
        .nodes
        .get(&parent)
        .and_then(|n| n.budget.as_ref())
        .is_some();
    if !has_parent_budget {
        return;
    }
    let a = cx
        .state
        .tree
        .nodes
        .get_mut(child)
        .and_then(|n| n.budget.as_mut())
        .expect("checked above");
    a.returned = true;
    let (limit, mut used) = (a.limit.clone(), a.used.clone());
    // What the child's own children still hold stays spent until they come back.
    add(&mut used, &a.allotted.clone());
    if let Some(pa) = cx
        .state
        .tree
        .nodes
        .get_mut(&parent)
        .and_then(|n| n.budget.as_mut())
    {
        sub(&mut pa.allotted, &limit);
        add(&mut pa.used, &used);
    }
    crate::tree::persist(cx, child);
    crate::tree::persist(cx, &parent);
    crate::sessions::changed(cx, &parent);
}
