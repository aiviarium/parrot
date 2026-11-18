//! Starts a command so that it dies with its parent: process groups, parent-death signals, job objects.
//!
//! `parrot guard -- <cmd> [args]` calls [`run`]. Callers spawn through it with [`guarded_command`].

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

pub mod script;

/// How long a forwarded SIGTERM or SIGINT has before the group is killed.
pub const GRACE: Duration = Duration::from_secs(10);

/// Runs `program` under the guard and returns the exit code the guard should exit with.
pub fn run(program: OsString, args: Vec<OsString>) -> std::io::Result<i32> {
    #[cfg(unix)]
    return unix::run(program, args);
    #[cfg(windows)]
    return windows::run(program, args);
}

/// Whether the current executable answers `guard`: `parrot` itself, or a standalone driver
/// binary named `parrot-driver-<name>`.
pub fn can_guard() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .is_some_and(|s| s == "parrot" || s.starts_with("parrot-driver-"))
}

/// A command that runs `program` through the current executable's `guard` subcommand.
pub fn guarded_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let exe = std::env::current_exe().unwrap_or_else(|_| "parrot".into());
    guarded_command_with(&exe, program)
}

/// A command that runs `program` through `guard_exe guard -- program`.
pub fn guarded_command_with(guard_exe: &Path, program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(guard_exe);
    cmd.arg("guard").arg("--").arg(program);
    cmd
}

/// Kills the process group led by `pid` (Unix). Elsewhere this does nothing; kill the child itself.
pub fn kill_tree(pid: u32) {
    #[cfg(unix)]
    unix::kill_group(pid as i32, libc::SIGKILL);
    #[cfg(not(unix))]
    let _ = pid;
}
