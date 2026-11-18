//! A driver child process: its own process group, a drained stderr, and a kill for the whole tree.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, watch};

/// How much of a driver's stderr is kept for diagnostics (spec 3.1).
pub const STDERR_KEEP: usize = 1 << 20;

/// The last bytes a process wrote to stderr, up to a fixed size.
#[derive(Debug, Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<u8>>>);

impl StderrTail {
    fn push(&self, bytes: &[u8]) {
        let mut buf = self.0.lock().unwrap();
        let bytes = &bytes[bytes.len().saturating_sub(STDERR_KEEP)..];
        let overflow = (buf.len() + bytes.len()).saturating_sub(STDERR_KEEP);
        buf.drain(..overflow);
        buf.extend(bytes);
    }

    /// The kept stderr, decoded lossily.
    pub fn text(&self) -> String {
        let buf = self.0.lock().unwrap();
        let (a, b) = buf.as_slices();
        let mut all = Vec::with_capacity(buf.len());
        all.extend_from_slice(a);
        all.extend_from_slice(b);
        String::from_utf8_lossy(&all).into_owned()
    }
}

/// A running child with piped stdio. Its exit status is published once it is reaped.
pub struct Process {
    pub pid: Option<u32>,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: StderrTail,
    exit: watch::Receiver<Option<ExitStatus>>,
    kill: Option<oneshot::Sender<()>>,
}

impl Process {
    /// Spawns `program` with `args` and extra `env`, in its own process group on Unix.
    pub fn spawn<I, S, E, K, V>(program: impl AsRef<OsStr>, args: I, env: E) -> std::io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let tail = StderrTail::default();
        if let Some(mut err) = child.stderr.take() {
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut chunk = vec![0u8; 64 * 1024];
                while let Ok(n) = err.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    tail.push(&chunk[..n]);
                }
            });
        }
        let (exit_tx, exit) = watch::channel(None);
        let (kill, kill_rx) = oneshot::channel();
        tokio::spawn(reap(child, pid, kill_rx, exit_tx));
        Ok(Self {
            pid,
            stdin,
            stdout,
            stderr: tail,
            exit,
            kill: Some(kill),
        })
    }

    /// A receiver that holds the exit status once the process has been reaped.
    pub fn exit(&self) -> watch::Receiver<Option<ExitStatus>> {
        self.exit.clone()
    }

    /// Kills the process and its group.
    pub fn kill(&mut self) {
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
    }

    /// Waits for the process to be reaped.
    pub async fn wait(&self) -> Option<ExitStatus> {
        let mut exit = self.exit.clone();
        exit.wait_for(Option::is_some).await.ok().and_then(|s| *s)
    }
}

async fn reap(
    mut child: Child,
    pid: Option<u32>,
    kill: oneshot::Receiver<()>,
    exit: watch::Sender<Option<ExitStatus>>,
) {
    let status = tokio::select! {
        status = child.wait() => status,
        Ok(()) = kill => {
            if let Some(pid) = pid {
                parrot_guard::kill_tree(pid);
            }
            let _ = child.start_kill();
            child.wait().await
        }
    };
    // Whatever the leader left behind in its group goes with it.
    if let Some(pid) = pid {
        parrot_guard::kill_tree(pid);
    }
    let _ = exit.send(Some(status.unwrap_or_else(|_| failed_status())));
}

fn failed_status() -> ExitStatus {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::from_raw(1 << 8)
    }
    #[cfg(windows)]
    {
        std::os::windows::process::ExitStatusExt::from_raw(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_the_last_mebibyte() {
        let tail = StderrTail::default();
        tail.push(&vec![b'a'; STDERR_KEEP]);
        tail.push(b"end");
        let text = tail.text();
        assert_eq!(text.len(), STDERR_KEEP);
        assert!(text.ends_with("aend"));
        tail.push(&vec![b'z'; STDERR_KEEP + 5]);
        assert!(tail.text().bytes().all(|b| b == b'z'));
    }
}
