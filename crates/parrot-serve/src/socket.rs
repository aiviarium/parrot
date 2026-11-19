//! The local socket (spec 3.3): a Unix socket in a private directory, or a named pipe on Windows.
//! Only the user who runs the runtime may connect, and connects as the owner.

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix {
    use crate::dispatch::Runtime;
    use crate::framing::serve_lines;
    use parrot_core::Peer;
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use tokio::net::UnixListener;

    /// The longest socket path macOS accepts.
    #[cfg(target_os = "macos")]
    const MAX_PATH: usize = 104;

    /// The socket path for this user and OS.
    pub fn default_socket_path() -> io::Result<PathBuf> {
        let home = || {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
        };
        #[cfg(target_os = "macos")]
        let path = home()?.join("Library/Application Support/parrot/sock");
        #[cfg(not(target_os = "macos"))]
        let path = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("parrot/sock"),
            _ => home()?.join(".local/state/parrot/sock"),
        };
        Ok(path)
    }

    /// Creates the socket's directory with mode 0700, removes a stale socket and binds.
    /// Fails if another runtime still answers on the path.
    pub fn bind_socket(path: &Path) -> io::Result<UnixListener> {
        #[cfg(target_os = "macos")]
        if path.as_os_str().len() > MAX_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("socket path {} is over {MAX_PATH} bytes", path.display()),
            ));
        }
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no directory"))?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let meta = std::fs::metadata(dir)?;
        if meta.uid() != uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is owned by another user", dir.display()),
            ));
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        if std::fs::symlink_metadata(path).is_ok() {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a runtime is already listening on {}", path.display()),
                ));
            }
            std::fs::remove_file(path)?;
        }
        UnixListener::bind(path)
    }

    fn uid() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    /// Accepts connections forever, refusing peers that run as another user.
    pub async fn serve_socket(rt: Runtime, listener: UnixListener) -> io::Result<()> {
        serve_uid(rt, listener, uid(), Peer::Owner).await
    }

    /// The controller connects as root to a socket created by systemd. The runtime
    /// itself can run unprivileged and never creates or unlinks the broker socket.
    #[cfg(target_os = "linux")]
    pub async fn serve_broker(rt: Runtime, listener: UnixListener) -> io::Result<()> {
        serve_uid(rt, listener, 0, broker_peer()).await
    }

    /// [`serve_broker`] accepting a peer of `peer_uid`, for a test that cannot run as root: it
    /// runs both the accept path and the reject path of the same broker.
    #[cfg(target_os = "linux")]
    #[doc(hidden)]
    pub async fn serve_broker_for_test(rt: Runtime, listener: UnixListener, peer_uid: u32) -> io::Result<()> {
        serve_uid(rt, listener, peer_uid, broker_peer()).await
    }

    /// What a host's Parrot may do: Rookery checks use on the host itself.
    #[cfg(target_os = "linux")]
    fn broker_peer() -> Peer {
        // Its clients can work with sessions but cannot administer links or tokens, act as a
        // session, or register client functions.
        Peer::Token {
            token: "rookery-broker".into(),
            scopes: [
                "read",
                "prompt",
                "answer",
                "configure",
                "manage",
                "delegate",
                "workspace",
            ]
            .map(str::to_owned)
            .to_vec(),
            sessions: None,
            provides: Some(vec![]),
        }
    }

    async fn serve_uid(rt: Runtime, listener: UnixListener, peer_uid: u32, peer: Peer) -> io::Result<()> {
        // A socket that cannot take a connection for a moment is waited out, so one aborted
        // connection or a short lack of descriptors does not end the runtime's socket.
        let mut backoff = std::time::Duration::from_millis(10);
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(taken) => {
                    backoff = std::time::Duration::from_millis(10);
                    taken
                }
                Err(error) if transient(&error) => {
                    eprintln!("parrot: cannot accept a connection: {error}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(1));
                    continue;
                }
                Err(error) => return Err(error),
            };
            // `peer_cred` is SO_PEERCRED on Linux and getpeereid on macOS.
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == peer_uid => {}
                _ => continue,
            }
            let rt = rt.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                let (r, w) = stream.into_split();
                let _ = serve_lines(rt, peer, r, w).await;
            });
        }
    }

    /// Whether an `accept` error is one to wait out: a connection the peer ended before it was
    /// taken, a signal, or no descriptor for the moment. A listener that is no socket any more
    /// stays an error.
    fn transient(error: &io::Error) -> bool {
        use rustix::io::Errno;
        matches!(
            error.kind(),
            io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) || [Errno::MFILE, Errno::NFILE, Errno::NOBUFS, Errno::NOMEM]
            .into_iter()
            .any(|errno| error.raw_os_error() == Some(errno.raw_os_error()))
    }
}

#[cfg(windows)]
mod windows {
    use crate::dispatch::Runtime;
    use crate::framing::serve_lines;
    use parrot_core::Peer;
    use std::io;

    pub use parrot_link::pipe::{default_pipe_name, user_sid};

    /// Serves the named pipe forever. The ACL admits only the current user, so every client is
    /// the owner.
    pub async fn serve_pipe(rt: Runtime, name: &str) -> io::Result<()> {
        let mut listener = parrot_link::pipe::Listener::bind(name)?;
        loop {
            let connected = listener.accept().await?;
            let rt = rt.clone();
            tokio::spawn(async move {
                let (r, w) = tokio::io::split(connected);
                let _ = serve_lines(rt, Peer::Owner, r, w).await;
            });
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::windows::{default_pipe_name, user_sid};

    #[test]
    fn the_pipe_is_named_for_the_current_user() {
        let sid = user_sid().expect("the process has a user token");
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert_eq!(default_pipe_name().unwrap(), format!(r"\\.\pipe\parrot-{sid}"));
    }
}
