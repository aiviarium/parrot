//! Runs the core's effects: messages go to connection writers, the rest to a `Backend`.

use crate::dispatch::Cmd;
use parrot_core::{BindingId, Clock, ConnId, Effect, Input, Peer};
use parrot_proto::rpc::{Notification, Request, Response};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Notify, mpsc};

/// Drivers and storage, which the integrator supplies. Calls must not block: start the work and
/// feed answers back through `Runtime::input`.
pub trait Backend: Send + 'static {
    fn start_binding(&mut self, binding: BindingId, agent: String, start: Value);
    fn driver_call(&mut self, binding: BindingId, call: u64, method: String, params: Value);
    fn stop_binding(&mut self, binding: BindingId);
    fn persist(&mut self, record: Value);
    /// Returns once every record persisted so far is written.
    fn sync(&mut self) {}
    /// Run a workspace operation; answer with `Input::WorkspaceReply`.
    fn workspace(&mut self, _call: u64, _op: String, _params: Value) {}
    /// Told of every timer; the executor schedules the `Input::Tick` itself.
    fn timer_at(&mut self, _at_ms: u64) {}
    /// Call a driver without a binding (`agent/list`); answer with `Input::AgentReply`.
    fn agent_call(&mut self, _agent: String, _call: u64, _method: String, _params: Value) {}
}

/// A backend that does nothing.
#[derive(Debug, Default)]
pub struct NoopBackend;

impl Backend for NoopBackend {
    fn start_binding(&mut self, _: BindingId, _: String, _: Value) {}
    fn driver_call(&mut self, _: BindingId, _: u64, _: String, _: Value) {}
    fn stop_binding(&mut self, _: BindingId) {}
    fn persist(&mut self, _: Value) {}
}

/// A backend that keeps the effects it gets, for tests.
#[derive(Debug, Default, Clone)]
pub struct RecordingBackend(pub Arc<Mutex<Vec<Effect>>>);

impl RecordingBackend {
    fn push(&self, effect: Effect) {
        self.0.lock().expect("not poisoned").push(effect);
    }
}

impl Backend for RecordingBackend {
    fn start_binding(&mut self, binding: BindingId, agent: String, start: Value) {
        self.push(Effect::StartBinding {
            binding,
            agent,
            start,
        });
    }
    fn driver_call(&mut self, binding: BindingId, call: u64, method: String, params: Value) {
        self.push(Effect::DriverCall {
            binding,
            call,
            method,
            params,
        });
    }
    fn stop_binding(&mut self, binding: BindingId) {
        self.push(Effect::StopBinding { binding });
    }
    fn persist(&mut self, record: Value) {
        self.push(Effect::Persist { record });
    }
    fn timer_at(&mut self, at_ms: u64) {
        self.push(Effect::TimerAt { at_ms });
    }
    fn agent_call(&mut self, agent: String, call: u64, method: String, params: Value) {
        self.push(Effect::AgentCall {
            agent,
            call,
            method,
            params,
        });
    }
}

pub(crate) struct ConnHandle {
    pub peer: Peer,
    pub tx: mpsc::Sender<String>,
    pub kill: Arc<Notify>,
}

pub(crate) struct Executor {
    conns: HashMap<ConnId, ConnHandle>,
    backend: Box<dyn Backend>,
    clock: Box<dyn Clock>,
    inbox: mpsc::WeakSender<Cmd>,
}

impl Executor {
    pub fn new(backend: Box<dyn Backend>, clock: Box<dyn Clock>, inbox: mpsc::WeakSender<Cmd>) -> Self {
        Self {
            conns: HashMap::new(),
            backend,
            clock,
            inbox,
        }
    }

    pub fn add(&mut self, conn: ConnId, handle: ConnHandle) {
        self.conns.insert(conn, handle);
    }

    /// Drops the connection's queue; its writer sends what is queued, then the connection ends.
    pub fn remove(&mut self, conn: &ConnId) {
        self.conns.remove(conn);
    }

    pub fn connections(&self) -> Vec<(ConnId, Peer)> {
        let mut all: Vec<_> = self
            .conns
            .iter()
            .map(|(id, h)| (id.clone(), h.peer.clone()))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// Queues a line for a connection. A full queue means the client is too slow: it is closed.
    pub fn send(&mut self, conn: &ConnId, line: String) {
        let Some(handle) = self.conns.get(conn) else {
            return;
        };
        match handle.tx.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                handle.kill.notify_one();
                self.conns.remove(conn);
            }
            Err(TrySendError::Closed(_)) => {
                self.conns.remove(conn);
            }
        }
    }

    /// Execute one input's effects: its records are written first, so nothing the input caused
    /// reaches a client or driver before the store has it (spec 34).
    pub fn execute_all(&mut self, effects: Vec<Effect>) {
        let (records, rest): (Vec<_>, Vec<_>) = effects
            .into_iter()
            .partition(|e| matches!(e, Effect::Persist { .. }));
        if !records.is_empty() {
            for effect in records {
                self.execute(effect);
            }
            self.backend.sync();
        }
        for effect in rest {
            self.execute(effect);
        }
    }

    pub fn execute(&mut self, effect: Effect) {
        match effect {
            Effect::Reply { conn, id, result } => {
                let response = match result {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(id, e),
                };
                self.send(&conn, to_line(&response));
            }
            Effect::Notify { conn, method, params } => {
                self.send(&conn, to_line(&Notification::new(method, params)))
            }
            Effect::ClientCall {
                conn,
                call,
                method,
                params,
            } => self.send(&conn, to_line(&Request::new(call.into(), method, params))),
            Effect::Close { conn } => self.remove(&conn),
            Effect::StartBinding {
                binding,
                agent,
                start,
            } => self.backend.start_binding(binding, agent, start),
            Effect::DriverCall {
                binding,
                call,
                method,
                params,
            } => self.backend.driver_call(binding, call, method, params),
            Effect::StopBinding { binding } => self.backend.stop_binding(binding),
            Effect::Workspace { call, op, params } => self.backend.workspace(call, op, params),
            Effect::AgentCall {
                agent,
                call,
                method,
                params,
            } => self.backend.agent_call(agent, call, method, params),
            Effect::Persist { record } => self.backend.persist(record),
            Effect::TimerAt { at_ms } => {
                self.backend.timer_at(at_ms);
                self.schedule_tick(at_ms);
            }
        }
    }

    fn schedule_tick(&self, at_ms: u64) {
        let delay = Duration::from_millis(at_ms.saturating_sub(self.clock.now_ms()));
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(tx) = inbox.upgrade() {
                let _ = tx.send(Cmd::Input(Input::Tick { now_ms: at_ms })).await;
            }
        });
    }
}

fn to_line<T: serde::Serialize>(msg: &T) -> String {
    serde_json::to_string(msg).expect("protocol messages serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use parrot_core::clock::ManualClock;

    #[tokio::test]
    async fn overflowing_client_is_closed() {
        let (inbox, _rx) = mpsc::channel(1);
        let mut exec = Executor::new(
            Box::new(NoopBackend),
            Box::new(ManualClock::default()),
            inbox.downgrade(),
        );
        let (tx, mut rx) = mpsc::channel(1);
        let kill = Arc::new(Notify::new());
        let conn = "con_a".to_string();
        let handle = ConnHandle {
            peer: Peer::Owner,
            tx,
            kill: kill.clone(),
        };
        exec.add(conn.clone(), handle);
        exec.execute(Effect::ClientCall {
            conn: conn.clone(),
            call: 3,
            method: "function/call".into(),
            params: Value::Null,
        });
        let line: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(line["id"], 3);
        exec.send(&conn, "1".into());
        exec.send(&conn, "2".into());
        assert!(exec.connections().is_empty());
        tokio::time::timeout(Duration::from_secs(1), kill.notified())
            .await
            .unwrap();
    }
}
