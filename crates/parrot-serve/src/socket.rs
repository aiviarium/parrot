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
        loop {
            let (stream, _) = listener.accept().await?;
            // `peer_cred` is SO_PEERCRED on Linux and getpeereid on macOS.
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == uid() => {}
                _ => continue,
            }
            let rt = rt.clone();
            tokio::spawn(async move {
                let (r, w) = stream.into_split();
                let _ = serve_lines(rt, Peer::Owner, r, w).await;
            });
        }
    }
}

#[cfg(windows)]
mod windows {
    use crate::dispatch::Runtime;
    use crate::framing::serve_lines;
    use parrot_core::Peer;
    use std::ffi::c_void;
    use std::io;
    use std::ptr::null_mut;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The current user's SID as a string, such as `S-1-5-21-…`.
    pub fn user_sid() -> io::Result<String> {
        // SAFETY: plain Win32 calls on buffers sized as the API reports; every handle and
        // allocation is released before returning.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut wide: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut n = 0;
            while *wide.add(n) != 0 {
                n += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(wide, n));
            LocalFree(wide.cast());
            Ok(sid)
        }
    }

    /// `\\.\pipe\parrot-<user SID>`.
    pub fn default_pipe_name() -> io::Result<String> {
        Ok(format!(r"\\.\pipe\parrot-{}", user_sid()?))
    }

    /// A security descriptor that grants the current user alone full access.
    struct UserOnly(PSECURITY_DESCRIPTOR);

    impl UserOnly {
        fn new() -> io::Result<Self> {
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", user_sid()?)
                .encode_utf16()
                .chain([0])
                .collect();
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            // SAFETY: `sddl` is NUL-terminated; `sd` is freed in `Drop`.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut sd,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(sd))
        }

        fn create(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
            let mut attrs = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.0,
                bInheritHandle: 0,
            };
            // SAFETY: `attrs` and the descriptor it points to outlive the call.
            unsafe {
                ServerOptions::new()
                    .first_pipe_instance(first)
                    .reject_remote_clients(true)
                    .create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut c_void)
            }
        }
    }

    impl Drop for UserOnly {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    // SAFETY: the descriptor is only read after creation.
    unsafe impl Send for UserOnly {}
    unsafe impl Sync for UserOnly {}

    /// Serves the named pipe forever. The ACL admits only the current user, so every client is
    /// the owner.
    pub async fn serve_pipe(rt: Runtime, name: &str) -> io::Result<()> {
        let acl = UserOnly::new()?;
        let mut server = acl.create(name, true)?;
        loop {
            server.connect().await?;
            let connected = server;
            server = acl.create(name, false)?;
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
