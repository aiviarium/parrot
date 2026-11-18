//! PowerShell scripts, which Windows cannot start by itself: a package manager writes
//! `foo.ps1` beside `foo.cmd`, and `CreateProcess` needs a host to run it.

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Whether `program` is a PowerShell script.
pub fn is_powershell(program: &OsStr) -> bool {
    Path::new(program)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ps1"))
}

/// The arguments that run `script` under a PowerShell host, then `args` as the script's own.
///
/// The policy is bypassed because the shims a package manager installs are unsigned, which the
/// default policy refuses, and because the host is only ever given one file from `PATH`.
pub fn host_args(script: &OsStr, args: &[OsString]) -> Vec<OsString> {
    let mut out: Vec<OsString> = [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    out.push(script.to_owned());
    out.extend(args.iter().cloned());
    out
}

/// The program and arguments to start. A PowerShell script is run through a host, which is what
/// Windows needs; anything else is started as given.
pub fn prepare(program: std::path::PathBuf, args: Vec<OsString>) -> (std::path::PathBuf, Vec<OsString>) {
    if !is_powershell(program.as_os_str()) {
        return (program, args);
    }
    match host() {
        Some(host) => (host, host_args(program.as_os_str(), &args)),
        // Without a host the OS reports its own error, which says more than a made-up one.
        None => (program, args),
    }
}

/// The PowerShell on this machine: `pwsh` when installed, else Windows PowerShell.
#[cfg(windows)]
fn host() -> Option<std::path::PathBuf> {
    ["pwsh", "powershell"]
        .iter()
        .find_map(|name| crate::windows::resolve_program(OsStr::new(name)))
}

#[cfg(not(windows))]
fn host() -> Option<std::path::PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ps1_file_needs_a_host_and_other_files_do_not() {
        assert!(is_powershell(OsStr::new(r"C:\npm\codex.ps1")));
        assert!(is_powershell(OsStr::new("codex.PS1")));
        assert!(!is_powershell(OsStr::new("codex.cmd")));
        assert!(!is_powershell(OsStr::new("codex")));
    }

    #[test]
    fn a_program_that_is_not_a_script_is_started_as_given() {
        let program = std::path::PathBuf::from("codex");
        let (started, args) = prepare(program.clone(), vec![OsString::from("chat")]);
        assert_eq!(started, program);
        assert_eq!(args, [OsString::from("chat")]);
    }

    #[test]
    fn the_host_runs_the_script_with_its_own_arguments() {
        let args = host_args(
            OsStr::new(r"C:\npm\codex.ps1"),
            &[OsString::from("exec"), OsString::from("--json")],
        );
        let text: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            text,
            [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                r"C:\npm\codex.ps1",
                "exec",
                "--json",
            ]
        );
    }
}
