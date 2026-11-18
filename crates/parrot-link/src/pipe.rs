//! A named pipe that only the current user can reach, which stands in for a Unix socket on
//! Windows. The ACL is set when the pipe is created, so a client needs no check of its own.

use std::ffi::c_void;
use std::io;
use std::path::Path;
use std::ptr::null_mut;

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
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

/// A pipe that accepts connections one at a time, keeping one instance waiting.
pub struct Listener {
    name: String,
    acl: UserOnly,
    pending: Option<NamedPipeServer>,
}

impl Listener {
    /// Binds `name`, such as `\\.\pipe\parrot-…`, refusing a name another process already serves.
    pub fn bind(name: &str) -> io::Result<Self> {
        let acl = UserOnly::new()?;
        let server = acl.create(name, true)?;
        Ok(Self {
            name: name.to_string(),
            acl,
            pending: Some(server),
        })
    }

    /// The next connection. A new instance is created before the connected one is handed out,
    /// so a client never finds the pipe full.
    pub async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        let server = self
            .pending
            .take()
            .ok_or_else(|| io::Error::other("the pipe has no waiting instance"))?;
        server.connect().await?;
        self.pending = Some(self.acl.create(&self.name, false)?);
        Ok(server)
    }
}

/// Connects to the pipe `path` names.
pub fn connect(path: &Path) -> io::Result<NamedPipeClient> {
    tokio::net::windows::named_pipe::ClientOptions::new().open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_is_named_for_the_current_user() {
        let sid = user_sid().expect("the process has a user token");
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert_eq!(default_pipe_name().unwrap(), format!(r"\\.\pipe\parrot-{sid}"));
    }

    #[tokio::test]
    async fn a_listener_accepts_a_client() {
        let name = format!(r"\\.\pipe\parrot-test-{}-{:x}", std::process::id(), line!());
        let mut listener = Listener::bind(&name).unwrap();
        let client = tokio::spawn(async move { connect(Path::new(&name)) });
        let server = listener.accept().await.unwrap();
        assert!(client.await.unwrap().is_ok());
        drop(server);
    }
}
