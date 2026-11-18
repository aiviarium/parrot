//! The runtime's real [`Backend`]: process drivers and the SQLite store.
//!
//! # Drivers
//!
//! Each binding gets its own driver process, owned by one task. `StartBinding` spawns it, sends
//! `initialize` and then `binding/start`, whose answer is fed back as `Input::DriverReply` with a
//! call id from [`RuntimeBackend`]'s own counter. Those ids count down from `u64::MAX`, while the
//! core's own call ids count up from 1, so the two never meet. Calls to one binding run one at a
//! time, in order. Every `update` notification becomes `Input::DriverUpdate`, and updates a driver
//! sends before answering a call are fed in before the answer. A driver that exits without being
//! stopped is reported as `Input::DriverExited`.
//!
//! # Persist records
//!
//! `Effect::Persist` carries a JSON object tagged by `op`. Records are written in order on one
//! writer thread:
//!
//! | `op` | Fields | Store call |
//! | --- | --- | --- |
//! | `append` | `seq`, `session`?, `item`?, `at`, `update` | `append`, or `append_item` when `item` is set (then `session` is required) |
//! | `put` | `kind`, `id`, `value` | `put` |
//! | `delete` | `kind`, `id` | `delete` |
//! | `intent.begin` | `intent` (the core's id), `kind`, `payload` | `begin` |
//! | `intent.step` | `intent`, `step`, `payload` | `step` |
//! | `intent.finish` | `intent` | `finish` |
//! | `frame` | `binding`, `idx`, `at`, `dataVersion`, `frame` | `put_frame` |
//!
//! `at` is milliseconds since the epoch. The writer maps the core's intent ids to the store's for
//! the life of the process. A record with an unknown `op` or missing fields is logged to stderr
//! and dropped.

use crate::dispatch::Runtime;
use crate::executor::Backend;
use parrot_core::{BindingId, Input};
use parrot_driver::host::ProcessDriver;
use parrot_driver::traits::{Driver, DriverUpdate};
use parrot_proto::rpc::RpcError;
use parrot_store::{Ops, Store};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// The error code sent back when a driver cannot be started or reached.
const DRIVER_FAILED: i64 = -32000;

/// How to run a driver: the program and its arguments.
#[derive(Debug, Clone)]
struct DriverCommand {
    program: PathBuf,
    args: Vec<String>,
}

/// Where driver inputs go: the runtime, or a channel in tests.
#[derive(Clone)]
enum Sink {
    Runtime(Runtime),
    #[cfg(test)]
    Channel(mpsc::UnboundedSender<Input>),
}

impl Sink {
    async fn input(&self, input: Input) -> std::io::Result<()> {
        match self {
            Sink::Runtime(rt) => rt.input(input).await,
            #[cfg(test)]
            Sink::Channel(tx) => tx
                .send(input)
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
        }
    }
}

enum BindingCmd {
    Call {
        call: u64,
        method: String,
        params: Value,
    },
    Stop,
}

/// Runs drivers as child processes and writes persist records to the store.
pub struct RuntimeBackend {
    rt: Sink,
    commands: HashMap<String, DriverCommand>,
    bindings: HashMap<BindingId, mpsc::UnboundedSender<BindingCmd>>,
    next_call: u64,
    writer: Option<std::sync::mpsc::Sender<Value>>,
}

impl RuntimeBackend {
    /// A backend that feeds driver traffic into `rt` and writes records to `store`.
    pub fn new(rt: Runtime, store: Store) -> Self {
        Self::with_sink(Sink::Runtime(rt), store)
    }

    fn with_sink(rt: Sink, store: Store) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Value>();
        std::thread::Builder::new()
            .name("parrot-store".into())
            .spawn(move || {
                let mut intents = HashMap::new();
                for record in rx {
                    if let Err(err) = write_record(&store, &mut intents, &record) {
                        eprintln!("parrot: dropped persist record: {err}");
                    }
                }
            })
            .expect("can spawn the store thread");
        Self {
            rt,
            commands: HashMap::new(),
            bindings: HashMap::new(),
            next_call: 0,
            writer: Some(tx),
        }
    }

    /// Runs `agent`'s driver as `program args…` instead of the built-in resolution.
    pub fn with_driver_command(
        mut self,
        agent: impl Into<String>,
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.commands.insert(
            agent.into(),
            DriverCommand {
                program: program.into(),
                args: args.into_iter().map(Into::into).collect(),
            },
        );
        self
    }

    fn resolve(&self, agent: &str) -> Result<DriverCommand, String> {
        if let Some(cmd) = self.commands.get(agent) {
            return Ok(cmd.clone());
        }
        match agent {
            "claude" => {
                let program = std::env::current_exe().map_err(|e| format!("cannot find parrot: {e}"))?;
                Ok(DriverCommand {
                    program,
                    args: vec!["driver".into(), "claude".into()],
                })
            }
            _ => Err(format!("no driver for agent {agent}")),
        }
    }

    /// An internal call id, counting down from `u64::MAX`.
    fn internal_call(&mut self) -> u64 {
        let id = u64::MAX - self.next_call;
        self.next_call += 1;
        id
    }
}

impl Drop for RuntimeBackend {
    fn drop(&mut self) {
        // Closing the channel lets the writer thread finish what is queued and exit.
        self.writer.take();
    }
}

impl Backend for RuntimeBackend {
    fn start_binding(&mut self, binding: BindingId, agent: String, start: Value) {
        let call = self.internal_call();
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(old) = self.bindings.insert(binding.clone(), tx) {
            let _ = old.send(BindingCmd::Stop);
        }
        let rt = self.rt.clone();
        match self.resolve(&agent) {
            Ok(cmd) => {
                tokio::spawn(run_binding(rt, binding, cmd, start, call, rx));
            }
            Err(detail) => {
                self.bindings.remove(&binding);
                tokio::spawn(async move {
                    let _ = rt
                        .input(Input::DriverReply {
                            binding: binding.clone(),
                            call,
                            result: Err(RpcError::new(DRIVER_FAILED, detail.clone())),
                        })
                        .await;
                    let _ = rt.input(Input::DriverExited { binding, detail }).await;
                });
            }
        }
    }

    fn driver_call(&mut self, binding: BindingId, call: u64, method: String, params: Value) {
        let sent = self
            .bindings
            .get(&binding)
            .is_some_and(|tx| tx.send(BindingCmd::Call { call, method, params }).is_ok());
        if !sent {
            self.bindings.remove(&binding);
            let rt = self.rt.clone();
            tokio::spawn(async move {
                let result = Err(RpcError::new(DRIVER_FAILED, "binding has no running driver"));
                let _ = rt
                    .input(Input::DriverReply {
                        binding,
                        call,
                        result,
                    })
                    .await;
            });
        }
    }

    fn stop_binding(&mut self, binding: BindingId) {
        if let Some(tx) = self.bindings.remove(&binding) {
            let _ = tx.send(BindingCmd::Stop);
        }
    }

    fn persist(&mut self, record: Value) {
        if let Some(writer) = &self.writer {
            let _ = writer.send(record);
        }
    }
}

/// Owns one binding's driver process until it is stopped or exits.
async fn run_binding(
    rt: Sink,
    binding: BindingId,
    cmd: DriverCommand,
    start: Value,
    start_call: u64,
    mut cmds: mpsc::UnboundedReceiver<BindingCmd>,
) {
    let no_env: [(&str, &str); 0] = [];
    let (mut driver, mut updates) = match ProcessDriver::spawn(&cmd.program, &cmd.args, no_env) {
        Ok(spawned) => spawned,
        Err(err) => {
            let detail = format!("cannot start {}: {err}", cmd.program.display());
            let _ = rt
                .input(Input::DriverReply {
                    binding: binding.clone(),
                    call: start_call,
                    result: Err(RpcError::new(DRIVER_FAILED, detail.clone())),
                })
                .await;
            let _ = rt.input(Input::DriverExited { binding, detail }).await;
            return;
        }
    };
    let mut link = Link {
        rt: &rt,
        binding: &binding,
        updates: &mut updates,
        open: true,
    };
    let init = json!({
        "protocol": [1],
        "peer": { "kind": "runtime", "name": "parrot", "version": env!("CARGO_PKG_VERSION") },
        "features": {},
    });
    let mut result = link.call(&mut driver, "initialize", init).await;
    if result.is_ok() {
        result = link.call(&mut driver, "binding/start", start).await;
    }
    let _ = rt
        .input(Input::DriverReply {
            binding: binding.clone(),
            call: start_call,
            result,
        })
        .await;
    loop {
        if !link.open {
            break;
        }
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(BindingCmd::Call { call, method, params }) => {
                    let result = link.call(&mut driver, &method, params).await;
                    let _ = rt.input(Input::DriverReply { binding: binding.clone(), call, result }).await;
                }
                Some(BindingCmd::Stop) | None => {
                    driver.stop().await;
                    return;
                }
            },
            update = link.updates.recv() => match update {
                Some(update) => link.forward(update).await,
                None => link.open = false,
            },
        }
    }
    let mut detail = driver.stderr();
    if detail.is_empty() {
        detail = "driver exited".into();
    }
    let _ = rt.input(Input::DriverExited { binding, detail }).await;
}

/// A binding's update stream, forwarded to the runtime while calls are in flight.
struct Link<'a> {
    rt: &'a Sink,
    binding: &'a BindingId,
    updates: &'a mut mpsc::UnboundedReceiver<DriverUpdate>,
    /// False once the driver's stdout has ended.
    open: bool,
}

impl Link<'_> {
    async fn forward(&self, update: Value) {
        let _ = self
            .rt
            .input(Input::DriverUpdate {
                binding: self.binding.clone(),
                update,
            })
            .await;
    }

    /// Calls the driver, feeding in updates that arrive meanwhile and those sent before the answer.
    async fn call(
        &mut self,
        driver: &mut ProcessDriver,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let fut = driver.call(method, params);
        tokio::pin!(fut);
        let result = loop {
            tokio::select! {
                biased;
                update = self.updates.recv(), if self.open => match update {
                    Some(update) => self.forward(update).await,
                    None => self.open = false,
                },
                result = &mut fut => break result,
            }
        };
        while let Ok(update) = self.updates.try_recv() {
            self.forward(update).await;
        }
        result
    }
}

fn write_record(store: &Store, intents: &mut HashMap<u64, u64>, r: &Value) -> Result<(), String> {
    let str_field = |k: &str| r.get(k).and_then(Value::as_str);
    let u64_field = |k: &str| r.get(k).and_then(Value::as_u64);
    let need_str = |k: &str| str_field(k).ok_or_else(|| format!("{k} is missing in {r}"));
    let need_u64 = |k: &str| u64_field(k).ok_or_else(|| format!("{k} is missing in {r}"));
    let field = |k: &str| r.get(k).cloned().unwrap_or(Value::Null);
    let op = need_str("op")?;
    let done = match op {
        "append" => {
            let (seq, at) = (need_u64("seq")?, need_u64("at")?);
            let update = field("update");
            match (str_field("session"), str_field("item")) {
                (Some(session), Some(item)) => store.append_item(seq, session, item, at, &update),
                (None, Some(_)) => return Err(format!("item without session in {r}")),
                (session, None) => store.append(seq, session, at, &update),
            }
        }
        "put" => store.put(need_str("kind")?, need_str("id")?, &field("value")),
        "delete" => store.delete(need_str("kind")?, need_str("id")?).map(|_| ()),
        "intent.begin" => {
            let core_id = need_u64("intent")?;
            store.begin(need_str("kind")?, &field("payload")).map(|id| {
                intents.insert(core_id, id);
            })
        }
        "intent.step" => {
            let id = intent_id(intents, need_u64("intent")?)?;
            store.step(id, need_str("step")?, &field("payload"))
        }
        "intent.finish" => {
            let core_id = need_u64("intent")?;
            let id = intent_id(intents, core_id)?;
            intents.remove(&core_id);
            store.finish(id).map(|_| ())
        }
        "frame" => store.put_frame(
            need_str("binding")?,
            need_u64("idx")?,
            need_u64("at")?,
            need_str("dataVersion")?,
            &field("frame"),
        ),
        other => return Err(format!("unknown op {other}")),
    };
    done.map_err(|e| e.to_string())
}

fn intent_id(intents: &HashMap<u64, u64>, core_id: u64) -> Result<u64, String> {
    intents
        .get(&core_id)
        .copied()
        .ok_or_else(|| format!("intent {core_id} was not begun"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A driver in sh: answers calls 1 and 2 (`initialize`, `binding/start`, with an update before
    /// the second answer), echoes call 3, then exits when stdin ends.
    #[cfg(unix)]
    const FAKE_DRIVER: &str = r#"
read l; echo '{"jsonrpc":"2.0","id":1,"result":{"peer":{"kind":"driver","name":"sh"}}}'
read l; echo '{"jsonrpc":"2.0","method":"update","params":{"binding":"bnd_a","update":{"type":"state","state":"idle","background":[]}}}'
echo '{"jsonrpc":"2.0","id":2,"result":{"isolation":"none","level":"read","network":false,"options":[],"actions":[]}}'
read l; echo '{"jsonrpc":"2.0","id":3,"result":{"disposition":"started"}}'
read l; echo bye >&2
"#;

    async fn next(rx: &mut mpsc::UnboundedReceiver<Input>) -> Input {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("an input in time")
            .expect("the backend is alive")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn drives_a_process_driver() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backend = RuntimeBackend::with_sink(Sink::Channel(tx), Store::memory().unwrap())
            .with_driver_command("fake", "sh", ["-c", FAKE_DRIVER]);
        backend.start_binding("bnd_a".into(), "fake".into(), json!({"binding":"bnd_a"}));
        let Input::DriverUpdate { binding, update } = next(&mut rx).await else {
            panic!("expected the update first");
        };
        assert_eq!(binding, "bnd_a");
        assert_eq!(update["update"]["state"], "idle");
        let Input::DriverReply { call, result, .. } = next(&mut rx).await else {
            panic!("expected the start reply");
        };
        assert_eq!(call, u64::MAX);
        assert_eq!(result.unwrap()["level"], "read");

        backend.driver_call("bnd_a".into(), 4, "binding/prompt".into(), json!({}));
        let Input::DriverReply { call, result, .. } = next(&mut rx).await else {
            panic!("expected the prompt reply");
        };
        assert_eq!(call, 4);
        assert_eq!(result.unwrap()["disposition"], "started");

        // The next call makes the driver exit without answering.
        backend.driver_call("bnd_a".into(), 5, "binding/cancel".into(), json!({}));
        let Input::DriverReply { call, result, .. } = next(&mut rx).await else {
            panic!("expected the cancel reply");
        };
        assert_eq!(call, 5);
        assert!(result.is_err());
        let Input::DriverExited { detail, .. } = next(&mut rx).await else {
            panic!("expected the exit");
        };
        assert!(detail.contains("bye"), "{detail}");
    }

    #[tokio::test]
    async fn unknown_agent_fails_the_start() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backend = RuntimeBackend::with_sink(Sink::Channel(tx), Store::memory().unwrap());
        backend.start_binding("bnd_b".into(), "nobody".into(), json!({}));
        assert!(matches!(
            next(&mut rx).await,
            Input::DriverReply {
                call: u64::MAX,
                result: Err(_),
                ..
            }
        ));
        assert!(matches!(next(&mut rx).await, Input::DriverExited { .. }));
        backend.driver_call("bnd_b".into(), 9, "binding/prompt".into(), json!({}));
        assert!(matches!(
            next(&mut rx).await,
            Input::DriverReply {
                call: 9,
                result: Err(_),
                ..
            }
        ));
    }

    #[test]
    fn records_reach_the_store() {
        let store = Store::memory().unwrap();
        let mut intents = HashMap::new();
        let records = [
            json!({"op":"append","seq":1,"at":5,"update":{"type":"x"}}),
            json!({"op":"append","seq":2,"session":"ses_a","item":"itm_a","at":6,"update":{}}),
            json!({"op":"put","kind":"session","id":"ses_a","value":{"n":1}}),
            json!({"op":"put","kind":"session","id":"ses_b","value":{}}),
            json!({"op":"delete","kind":"session","id":"ses_b"}),
            json!({"op":"intent.begin","intent":7,"kind":"prompt","payload":{}}),
            json!({"op":"intent.step","intent":7,"step":"sent","payload":{}}),
            json!({"op":"intent.begin","intent":8,"kind":"wake","payload":{}}),
            json!({"op":"intent.finish","intent":8}),
        ];
        for r in &records {
            write_record(&store, &mut intents, r).unwrap();
        }
        assert!(write_record(&store, &mut intents, &json!({"op":"nope"})).is_err());
        assert!(write_record(&store, &mut intents, &json!({"op":"append","seq":3})).is_err());
        assert_eq!(store.head().unwrap(), Some(2));
        let rows = store.read_session("ses_a", 0, 10).unwrap();
        assert_eq!(rows[0].item.as_deref(), Some("itm_a"));
        assert_eq!(store.get("session", "ses_a").unwrap(), Some(json!({"n":1})));
        assert_eq!(store.get("session", "ses_b").unwrap(), None);
        let open = store.open_intents().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, "prompt");
        assert_eq!(open[0].steps[0].step, "sent");
    }
}
