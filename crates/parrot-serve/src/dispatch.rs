//! The runtime actor: one task owns the `Core` and takes inputs from every connection in order.

use crate::executor::{Backend, ConnHandle, Executor};
use crate::framing::Incoming;
use parrot_core::{Clock, ConnId, Core, Input, Peer};
use std::io;
use std::sync::Arc;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

/// Queue length of the actor's inbox.
const INBOX: usize = 1024;
/// Messages queued for one connection before it counts as too slow and is closed.
pub const WRITER_QUEUE: usize = 1024;

pub(crate) enum Cmd {
    Connect {
        conn: ConnId,
        peer: Peer,
        handle: ConnHandle,
    },
    Input(Input),
    /// A line for a connection that bypasses the core, such as a rejected batch.
    Raw {
        conn: ConnId,
        line: String,
    },
    Disconnect {
        conn: ConnId,
    },
    Connections(oneshot::Sender<Vec<(ConnId, Peer)>>),
}

/// A handle to the runtime actor. Clone it into every listener and backend.
#[derive(Clone)]
pub struct Runtime {
    pub(crate) tx: mpsc::Sender<Cmd>,
}

/// One connection's side of the actor: what to write, and a signal to drop it at once.
pub(crate) struct Connection {
    pub id: ConnId,
    pub rx: mpsc::Receiver<String>,
    pub kill: Arc<Notify>,
}

impl Runtime {
    /// Starts the actor. `backend` gets a handle so it can feed driver traffic back in. The task
    /// ends, returning the core, once every `Runtime` handle is dropped.
    pub fn spawn<B, F>(core: Core, clock: Box<dyn Clock>, backend: F) -> (Runtime, JoinHandle<Core>)
    where
        B: Backend,
        F: FnOnce(Runtime) -> B,
    {
        let (tx, rx) = mpsc::channel(INBOX);
        let rt = Runtime { tx };
        let backend = Box::new(backend(rt.clone()));
        let exec = Executor::new(backend, clock, rt.tx.downgrade());
        (rt, tokio::spawn(run(core, exec, rx)))
    }

    /// Feeds an input that doesn't come from a connection, such as `DriverUpdate`.
    pub async fn input(&self, input: Input) -> io::Result<()> {
        self.send(Cmd::Input(input)).await
    }

    /// The open connections and their peers.
    pub async fn connections(&self) -> io::Result<Vec<(ConnId, Peer)>> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Connections(tx)).await?;
        rx.await.map_err(|_| gone())
    }

    pub(crate) async fn connect(&self, peer: Peer) -> io::Result<Connection> {
        let (tx, rx) = mpsc::channel(WRITER_QUEUE);
        let kill = Arc::new(Notify::new());
        let conn = new_conn_id();
        let handle = ConnHandle {
            peer: peer.clone(),
            tx,
            kill: kill.clone(),
        };
        self.send(Cmd::Connect {
            conn: conn.clone(),
            peer,
            handle,
        })
        .await?;
        Ok(Connection { id: conn, rx, kill })
    }

    /// Passes one classified message on. Returns `false` when the actor is gone.
    pub(crate) async fn incoming(&self, conn: &ConnId, incoming: Incoming) -> bool {
        let cmd = match incoming {
            Incoming::Input(input) => Cmd::Input(input),
            Incoming::Reject(line) => Cmd::Raw {
                conn: conn.clone(),
                line,
            },
            Incoming::Ignore => return true,
        };
        self.send(cmd).await.is_ok()
    }

    pub(crate) async fn disconnect(&self, conn: &ConnId) {
        let _ = self.send(Cmd::Disconnect { conn: conn.clone() }).await;
    }

    async fn send(&self, cmd: Cmd) -> io::Result<()> {
        self.tx.send(cmd).await.map_err(|_| gone())
    }
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the runtime has stopped")
}

async fn run(mut core: Core, mut exec: Executor, mut rx: mpsc::Receiver<Cmd>) -> Core {
    while let Some(cmd) = rx.recv().await {
        let input = match cmd {
            Cmd::Connect { conn, peer, handle } => {
                exec.add(conn.clone(), handle);
                Input::Connected { conn, peer }
            }
            Cmd::Disconnect { conn } => {
                exec.remove(&conn);
                Input::Disconnected { conn }
            }
            Cmd::Raw { conn, line } => {
                exec.send(&conn, line);
                continue;
            }
            Cmd::Connections(reply) => {
                let _ = reply.send(exec.connections());
                continue;
            }
            Cmd::Input(input) => input,
        };
        for effect in core.handle(input) {
            exec.execute(effect);
        }
    }
    core
}

/// A new connection id: `con_` and a ULID-shaped encoding of a UUID v7.
pub fn new_conn_id() -> ConnId {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let v = uuid::Uuid::now_v7().as_u128();
    let mut id = String::from("con_");
    for i in 0..26 {
        id.push(ALPHABET[((v >> (125 - 5 * i)) & 31) as usize] as char);
    }
    id
}

#[cfg(test)]
mod tests {
    #[test]
    fn conn_ids_are_ulid_shaped_and_distinct() {
        let a = super::new_conn_id();
        let b = super::new_conn_id();
        assert_eq!(a.len(), 30);
        assert!(a.starts_with("con_"));
        assert_ne!(a, b);
    }
}
