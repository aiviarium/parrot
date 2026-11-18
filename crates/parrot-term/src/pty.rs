//! Running a command on a local PTY.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{mpsc, watch};

use crate::io::{BoxFuture, ByteSink, ByteSource};
use crate::screen::Size;

/// What to run.
#[derive(Clone, Debug)]
pub struct PtyCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Variables set on top of the inherited environment.
    pub env: BTreeMap<String, String>,
    /// Variables removed from the inherited environment.
    pub env_remove: Vec<String>,
    /// Start from an empty environment instead of inheriting this process's.
    pub env_clear: bool,
    pub size: Size,
}

impl PtyCommand {
    pub fn new(program: impl Into<String>) -> Self {
        let mut env = BTreeMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env,
            env_remove: Vec::new(),
            env_clear: false,
            size: Size::default(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// Starts from an empty environment; `TERM`, `COLORTERM` and variables set with
    /// [`PtyCommand::env`] are still set.
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }
}

/// How the process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: u32,
    /// The signal that killed it, if any, by name.
    pub signal: Option<String>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == 0 && self.signal.is_none()
    }
}

fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn other<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

/// A signal sent to a running process, named so that every OS spells it the same.
///
/// Unix sends the signal to the process. Windows has no signals: an interrupt is Ctrl+C on the
/// pseudoconsole, and the others end the process tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Terminate,
    Hangup,
}

impl Signal {
    #[cfg(unix)]
    fn native(self) -> rustix::process::Signal {
        match self {
            Signal::Interrupt => rustix::process::Signal::INT,
            Signal::Terminate => rustix::process::Signal::TERM,
            Signal::Hangup => rustix::process::Signal::HUP,
        }
    }
}

/// A running process on a PTY.
pub struct Pty {
    pid: Option<u32>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    /// The PTY input side, kept here so Windows can type Ctrl+C into the pseudoconsole.
    #[cfg_attr(not(windows), allow(dead_code))]
    control: Arc<Mutex<Box<dyn Write + Send>>>,
    exit: watch::Receiver<Option<ExitStatus>>,
}

/// Output of a PTY.
pub struct PtySource {
    rx: mpsc::Receiver<Vec<u8>>,
}

/// Input and size of a PTY.
pub struct PtySink {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

/// Starts `cmd` on a new PTY.
pub fn spawn(cmd: &PtyCommand) -> io::Result<(Pty, PtySource, PtySink)> {
    let system = native_pty_system();
    let pair = system.openpty(pty_size(cmd.size)).map_err(other)?;
    let mut builder = CommandBuilder::new(&cmd.program);
    builder.args(&cmd.args);
    if let Some(cwd) = &cmd.cwd {
        builder.cwd(cwd);
    }
    if cmd.env_clear {
        builder.env_clear();
    }
    for k in &cmd.env_remove {
        builder.env_remove(k);
    }
    for (k, v) in &cmd.env {
        builder.env(k, v);
    }
    let mut child = pair.slave.spawn_command(builder).map_err(other)?;
    drop(pair.slave);
    let pid = child.process_id();
    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(other)?;
    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(pair.master.take_writer().map_err(other)?));

    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    std::thread::Builder::new()
        .name("parrot-term-pty-read".into())
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    // EIO once the child side is closed.
                    Err(_) => break,
                }
            }
        })?;

    let (exit_tx, exit_rx) = watch::channel(None);
    std::thread::Builder::new()
        .name("parrot-term-pty-wait".into())
        .spawn(move || {
            let status = match child.wait() {
                Ok(s) => ExitStatus {
                    code: s.exit_code(),
                    signal: s.signal().map(str::to_string),
                },
                Err(_) => ExitStatus {
                    code: u32::MAX,
                    signal: None,
                },
            };
            let _ = exit_tx.send(Some(status));
        })?;

    Ok((
        Pty {
            pid,
            killer: Mutex::new(killer),
            control: writer.clone(),
            exit: exit_rx,
        },
        PtySource { rx },
        PtySink {
            writer,
            master: Arc::new(Mutex::new(pair.master)),
        },
    ))
}

impl Pty {
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Sends `signal` to the process, or as close to it as the OS allows.
    pub fn signal(&self, signal: Signal) -> io::Result<()> {
        #[cfg(unix)]
        {
            let pid = self
                .pid
                .and_then(|p| rustix::process::Pid::from_raw(p as i32))
                .ok_or_else(|| io::Error::other("no pid"))?;
            rustix::process::kill_process(pid, signal.native()).map_err(io::Error::from)
        }
        #[cfg(windows)]
        match signal {
            // A pseudoconsole has no console control handler to signal, so Ctrl+C is the byte
            // the terminal would send for it.
            Signal::Interrupt => {
                let mut w = self.control.lock().map_err(other)?;
                w.write_all(b"\x03")?;
                w.flush()
            }
            Signal::Terminate | Signal::Hangup => self.kill(),
        }
    }

    /// Kills the process.
    pub fn kill(&self) -> io::Result<()> {
        self.killer.lock().map_err(other)?.kill()
    }

    /// The exit status, if it has exited.
    pub fn try_wait(&self) -> Option<ExitStatus> {
        self.exit.borrow().clone()
    }

    /// Waits for the process to exit.
    pub async fn wait(&self) -> ExitStatus {
        let mut rx = self.exit.clone();
        loop {
            if let Some(s) = rx.borrow_and_update().clone() {
                return s;
            }
            if rx.changed().await.is_err() {
                return rx.borrow().clone().unwrap_or(ExitStatus {
                    code: u32::MAX,
                    signal: None,
                });
            }
        }
    }
}

impl ByteSource for PtySource {
    fn read(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move { Ok(self.rx.recv().await) })
    }
}

impl ByteSink for PtySink {
    fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        let writer = self.writer.clone();
        let data = data.to_vec();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut w = writer.lock().map_err(other)?;
                w.write_all(&data)?;
                w.flush()
            })
            .await
            .map_err(other)?
        })
    }

    fn resize(&self, size: Size) -> BoxFuture<'_, io::Result<()>> {
        let r = self
            .master
            .lock()
            .map_err(other)
            .and_then(|m| m.resize(pty_size(size)).map_err(other));
        Box::pin(async move { r })
    }
}
