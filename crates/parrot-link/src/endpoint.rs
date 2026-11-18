//! The runtime's link endpoint (E9): `link.sock` beside the local socket `sock`, in the same
//! private directory (spec 3.3). `parrot connect --link` relays to it; `tcp` reaches the same
//! handshake over a TCP listener. A stream accepted here is only a candidate until
//! [`crate::noise::Listener::accept`] authenticates it.

use std::path::{Path, PathBuf};

/// The file name of the link endpoint.
pub const LINK_SOCKET: &str = "link.sock";

/// `link.sock` in the directory of the local socket `sock`.
pub fn link_socket_path(sock: &Path) -> PathBuf {
    sock.with_file_name(LINK_SOCKET)
}

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::Path;
    use tokio::net::{UnixListener, UnixStream};

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
    pub async fn connect_link_socket(path: &Path) -> io::Result<UnixStream> {
        let dir = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "link socket path has no directory")
        })?;
        check_private_dir(dir)?;
        UnixStream::connect(path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_socket_sits_beside_sock() {
        assert_eq!(
            link_socket_path(Path::new("/run/user/1000/parrot/sock")),
            Path::new("/run/user/1000/parrot/link.sock")
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
