//! The runtime's link endpoint (E9): `link.sock` beside the local socket `sock`, in the same
//! private directory (spec 3.3). `parrot connect --link` relays to it; `tcp` reaches the same
//! handshake over a TCP listener. A stream accepted here is only a candidate until
//! [`crate::noise::Listener::accept`] authenticates it.

use std::io;
use std::path::{Path, PathBuf};

/// The file name of the link endpoint.
pub const LINK_SOCKET: &str = "link.sock";

/// `link.sock` in the directory of the local socket `sock`, or the local pipe's name with
/// `-link` on Windows.
pub fn link_socket_path(sock: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        sock.with_file_name(LINK_SOCKET)
    }
    #[cfg(windows)]
    {
        let name = sock
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "parrot".into());
        sock.with_file_name(format!("{name}-link"))
    }
}

/// A link endpoint that accepts candidate connections.
#[cfg(unix)]
pub type LinkListener = tokio::net::UnixListener;

/// A candidate connection, from either end. Only the handshake makes it a link: a Unix socket,
/// or a named pipe's server or client end.
pub enum LinkStream {
    #[cfg(unix)]
    Socket(tokio::net::UnixStream),
    #[cfg(windows)]
    Server(tokio::net::windows::named_pipe::NamedPipeServer),
    #[cfg(windows)]
    Client(tokio::net::windows::named_pipe::NamedPipeClient),
}

impl std::fmt::Debug for LinkStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkStream")
    }
}

impl tokio::io::AsyncRead for LinkStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            #[cfg(windows)]
            Self::Server(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            #[cfg(windows)]
            Self::Client(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for LinkStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
            #[cfg(windows)]
            Self::Server(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
            #[cfg(windows)]
            Self::Client(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            #[cfg(windows)]
            Self::Server(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            #[cfg(windows)]
            Self::Client(stream) => std::pin::Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            #[cfg(windows)]
            Self::Server(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            #[cfg(windows)]
            Self::Client(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// Accepts the next connection on `listener`.
pub async fn accept_link(listener: &mut LinkListener) -> io::Result<LinkStream> {
    #[cfg(unix)]
    {
        Ok(LinkStream::Socket(accept_same_user(listener).await?))
    }
    #[cfg(windows)]
    {
        Ok(LinkStream::Server(listener.accept().await?))
    }
}

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::Path;
    use tokio::net::{UnixListener, UnixStream};

    use super::LinkStream;
    fn uid() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    /// Fails unless `dir` is a directory owned by this user with mode `0700`.
    pub fn check_private_dir(dir: &Path) -> io::Result<()> {
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not a directory", dir.display()),
            ));
        }
        if meta.uid() != uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is owned by another user", dir.display()),
            ));
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must have mode 0700", dir.display()),
            ));
        }
        Ok(())
    }

    /// Creates the endpoint's directory with mode `0700`, removes a stale socket and binds, as
    /// the local socket does. Fails if a runtime still answers on the path.
    pub fn bind_link_socket(path: &Path) -> io::Result<UnixListener> {
        let dir = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "link socket path has no directory")
        })?;
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

    /// The next connection from a process of this user; others are dropped unanswered.
    pub async fn accept_same_user(listener: &UnixListener) -> io::Result<UnixStream> {
        loop {
            let (stream, _) = listener.accept().await?;
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == uid() => return Ok(stream),
                _ => continue,
            }
        }
    }

    /// Connects to the link endpoint after checking its directory, so a relay never writes to a
    /// socket another user could have placed.
    pub async fn connect_link_socket(path: &Path) -> io::Result<LinkStream> {
        let dir = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "link socket path has no directory")
        })?;
        check_private_dir(dir)?;
        Ok(LinkStream::Socket(UnixStream::connect(path).await?))
    }
}

#[cfg(windows)]
pub use windows::*;

#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::Path;

    use super::{LinkListener, LinkStream};

    /// The pipe's ACL admits only this user, so its folder needs no check of its own.
    pub fn check_private_dir(_dir: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Creates the link pipe, refusing the name when a runtime already serves it.
    pub fn bind_link_socket(path: &Path) -> io::Result<LinkListener> {
        crate::pipe::Listener::bind(&path.to_string_lossy())
    }

    /// Connects to the link pipe. The ACL on the server end decides who may.
    pub async fn connect_link_socket(path: &Path) -> io::Result<LinkStream> {
        Ok(LinkStream::Client(crate::pipe::connect(path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn link_socket_sits_beside_sock() {
        assert_eq!(
            link_socket_path(Path::new("/run/user/1000/parrot/sock")),
            Path::new("/run/user/1000/parrot/link.sock")
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_link_pipe_sits_beside_the_local_pipe() {
        assert_eq!(
            link_socket_path(Path::new(r"\\.\pipe\parrot-S-1-5-21-1-2-3-500")),
            Path::new(r"\\.\pipe\parrot-S-1-5-21-1-2-3-500-link")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_a_shared_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        let path = run.join(LINK_SOCKET);
        let _listener = bind_link_socket(&path).unwrap();
        assert!(connect_link_socket(&path).await.is_ok());
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = connect_link_socket(&path).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
