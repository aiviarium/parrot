//! Targets that speak JSON-RPC lines: a child process on stdio, and a local socket.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use crate::Target;

/// How long a target waits for its next message before it counts as silent.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10);

/// One JSON message per line in each direction; a thread reads the incoming side.
pub struct Lines {
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<Value>,
    /// How long [`Target::recv`] waits.
    pub wait: Duration,
    /// Lines that were not JSON, for the report.
    pub garbage: mpsc::Receiver<String>,
}

impl Lines {
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let (bad, garbage) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = bad.send(line);
                    }
                }
            }
        });
        Lines {
            writer: Box::new(writer),
            rx,
            wait: DEFAULT_WAIT,
            garbage,
        }
    }
}

impl Target for Lines {
    fn send(&mut self, msg: Value) {
        let mut line = msg.to_string();
        line.push('\n');
        let _ = self.writer.write_all(line.as_bytes());
        let _ = self.writer.flush();
    }

    fn recv(&mut self) -> Option<Value> {
        self.rx.recv_timeout(self.wait).ok()
    }
}

/// A driver started as a child process, killed on drop.
pub struct Process {
    child: Child,
    lines: Option<Lines>,
}

impl Process {
    pub fn spawn(command: &[String], env: &[(OsString, OsString)]) -> std::io::Result<Self> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no command"))?;
        let mut child = Command::new(program)
            .args(args)
            .envs(env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        Ok(Process {
            child,
            lines: Some(Lines::new(stdout, stdin)),
        })
    }

    pub fn set_wait(&mut self, wait: Duration) {
        if let Some(l) = &mut self.lines {
            l.wait = wait;
        }
    }

    /// Closes stdin and gives the process a moment to stop before it is killed.
    pub fn finish(mut self, grace: Duration) {
        self.lines = None;
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Target for Process {
    fn send(&mut self, msg: Value) {
        if let Some(l) = &mut self.lines {
            l.send(msg);
        }
    }

    fn recv(&mut self) -> Option<Value> {
        self.lines.as_mut()?.recv()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.lines = None;
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// A connection to a runtime's local socket.
#[cfg(unix)]
pub fn socket(path: &std::path::Path) -> std::io::Result<Lines> {
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    let reader = stream.try_clone()?;
    Ok(Lines::new(reader, stream))
}
