//! A process driver behind the [`Driver`] trait: JSON-RPC lines on the child's stdin and stdout.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use parrot_proto::rpc::{Message, Request, Response, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use crate::process::{Process, StderrTail};
use crate::traits::{Driver, DriverUpdate};

/// The longest line accepted from a driver (spec 3.1).
pub const LINE_LIMIT: usize = 32 << 20;
/// How long `stop` waits for the driver to exit before killing its tree (spec 30.5).
pub const STOP_GRACE: Duration = Duration::from_secs(10);
/// The error code of a call that cannot complete because the driver is gone.
pub const DRIVER_EXITED: i64 = -32000;

type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>>;

/// A driver running as a child process.
pub struct ProcessDriver {
    process: Process,
    stdin: Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
    pending: Pending,
    next_id: AtomicU64,
    bindings: Arc<Mutex<BTreeSet<String>>>,
}

impl ProcessDriver {
    /// Starts the driver and returns it with the stream of its `update` notifications.
    pub fn spawn<I, S, E, K, V>(
        program: impl AsRef<OsStr>,
        args: I,
        env: E,
    ) -> std::io::Result<(Self, mpsc::UnboundedReceiver<DriverUpdate>)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut process = Process::spawn(program, args, env)?;
        let stdin = Arc::new(tokio::sync::Mutex::new(process.stdin.take()));
        let stdout = process.stdout.take().expect("stdout is piped");
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (updates, rx) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(
            stdout,
            stdin.clone(),
            pending.clone(),
            updates,
            process.stderr.clone(),
            process.exit(),
        ));
        let driver = Self {
            process,
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            bindings: Arc::default(),
        };
        Ok((driver, rx))
    }

    /// The last mebibyte the driver wrote to stderr.
    pub fn stderr(&self) -> String {
        self.process.stderr.text()
    }

    /// The bindings started and not yet stopped.
    pub fn bindings(&self) -> Vec<String> {
        self.bindings.lock().unwrap().iter().cloned().collect()
    }

    /// Stops every binding, closes stdin, and kills the tree if it hasn't exited after 10 seconds.
    pub async fn stop(&mut self) -> Option<ExitStatus> {
        self.stop_with_grace(STOP_GRACE).await
    }

    /// [`stop`](Self::stop) with a chosen grace period.
    pub async fn stop_with_grace(&mut self, grace: Duration) -> Option<ExitStatus> {
        let deadline = tokio::time::Instant::now() + grace;
        let stops = async {
            for binding in self.bindings() {
                let _ = self.call("binding/stop", json!({ "binding": binding })).await;
            }
        };
        let _ = tokio::time::timeout_at(deadline, stops).await;
        if let Some(mut stdin) = self.stdin.lock().await.take() {
            let _ = stdin.shutdown().await;
        }
        match tokio::time::timeout_at(deadline, self.process.wait()).await {
            Ok(status) => status,
            Err(_) => {
                self.process.kill();
                self.process.wait().await
            }
        }
    }

    async fn send(&self, line: String) -> Result<(), RpcError> {
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| exited("driver stdin is closed", None))?;
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        let written = async {
            pipe.write_all(&bytes).await?;
            pipe.flush().await
        };
        written
            .await
            .map_err(|e| exited(&format!("cannot write to driver: {e}"), None))
    }
}

impl Driver for ProcessDriver {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            let Some(pending) = pending.as_mut() else {
                return Err(exited("driver has exited", Some(&self.process.stderr)));
            };
            pending.insert(id, tx);
        }
        let binding = params.get("binding").and_then(Value::as_str).map(str::to_owned);
        let line =
            serde_json::to_string(&Request::new(json!(id), method, params)).expect("requests serialize");
        if let Err(err) = self.send(line).await {
            if let Some(p) = self.pending.lock().unwrap().as_mut() {
                p.remove(&id);
            }
            return Err(err);
        }
        let result = rx
            .await
            .unwrap_or_else(|_| Err(exited("driver has exited", Some(&self.process.stderr))))?;
        if let Some(binding) = binding {
            match method {
                "binding/start" => {
                    self.bindings.lock().unwrap().insert(binding);
                }
                "binding/stop" => {
                    self.bindings.lock().unwrap().remove(&binding);
                }
                _ => {}
            }
        }
        Ok(result)
    }
}

fn exited(message: &str, stderr: Option<&StderrTail>) -> RpcError {
    let err = RpcError::new(DRIVER_EXITED, message);
    match stderr {
        Some(tail) => err.with_data(json!({ "stderr": tail.text() })),
        None => err,
    }
}

async fn read_loop(
    stdout: ChildStdout,
    stdin: Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
    pending: Pending,
    updates: mpsc::UnboundedSender<DriverUpdate>,
    stderr: StderrTail,
    mut exit: tokio::sync::watch::Receiver<Option<ExitStatus>>,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let reason = loop {
        line.clear();
        let mut limited = (&mut reader).take(LINE_LIMIT as u64 + 1);
        match limited.read_until(b'\n', &mut line).await {
            Ok(0) => break "driver exited before replying".to_owned(),
            Ok(_) if line.len() > LINE_LIMIT => break "driver sent a line over the limit".into(),
            Ok(_) => {}
            Err(e) => break format!("cannot read from driver: {e}"),
        }
        let text = line.trim_ascii();
        if text.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Message>(text) {
            Ok(Message::Response(resp)) => {
                let tx = resp
                    .id
                    .as_u64()
                    .and_then(|id| pending.lock().unwrap().as_mut()?.remove(&id));
                if let Some(tx) = tx {
                    let _ = tx.send(match resp.error {
                        Some(err) => Err(err),
                        None => Ok(resp.result.unwrap_or(Value::Null)),
                    });
                }
            }
            Ok(Message::Notification(n)) if n.method == "update" => {
                let _ = updates.send(n.params);
            }
            Ok(Message::Notification(_)) => {}
            Ok(Message::Request(req)) => {
                // The runtime serves no methods to a process driver.
                let resp = Response::err(req.id, RpcError::new(-32601, "method not found"));
                let line = serde_json::to_string(&resp).expect("responses serialize") + "\n";
                if let Some(pipe) = stdin.lock().await.as_mut() {
                    let _ = pipe.write_all(line.as_bytes()).await;
                    let _ = pipe.flush().await;
                }
            }
            // A line that isn't JSON-RPC is skipped; stderr is where drivers log.
            Err(_) => {}
        }
    };
    // Give the reaper a moment so the error can name the exit status.
    let status = tokio::time::timeout(Duration::from_millis(500), exit.wait_for(Option::is_some))
        .await
        .ok()
        .and_then(|r| r.ok().and_then(|s| *s));
    let message = match status {
        Some(status) => format!("{reason} ({status})"),
        None => reason,
    };
    let waiting = pending.lock().unwrap().take().unwrap_or_default();
    for (_, tx) in waiting {
        let _ = tx.send(Err(exited(&message, Some(&stderr))));
    }
}
