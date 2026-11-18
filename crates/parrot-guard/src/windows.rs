//! The guard on Windows: a job object that kills the child tree when the guard's handle closes.

use std::ffi::{OsStr, OsString};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};

/// The extensions npm and bun shims use when `PATHEXT` is not set.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD;.PS1";

/// Finds a bare program name the way the shell would, including the `.cmd` and `.ps1` shims that
/// npm and bun install. A name with a directory is left to `CreateProcess`.
fn resolve(program: &OsStr) -> Option<PathBuf> {
    let name = program.to_string_lossy();
    if name.contains('\\') || name.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let pathext = std::env::var_os("PATHEXT").unwrap_or_else(|| DEFAULT_PATHEXT.into());
    resolve_in(program, &path, &pathext)
}

fn resolve_in(program: &OsStr, path: &OsStr, pathext: &OsStr) -> Option<PathBuf> {
    let name = program.to_string_lossy();
    if name.contains('\\') || name.contains('/') {
        return None;
    }
    let mut names = vec![program.to_owned()];
    if Path::new(program).extension().is_none() {
        for ext in pathext.to_string_lossy().split(';').filter(|e| !e.is_empty()) {
            let mut name = program.to_owned();
            name.push(ext);
            names.push(name);
        }
    }
    for dir in std::env::split_paths(path).filter(|dir| !dir.as_os_str().is_empty()) {
        for name in &names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn run(program: OsString, args: Vec<OsString>) -> std::io::Result<i32> {
    let program = resolve(&program).unwrap_or_else(|| PathBuf::from(&program));
    // SAFETY: null attributes and name create an anonymous job; the handle lives until the guard
    // exits, when the system closes it and kills the job.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: plain data, filled below.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: info is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the given size.
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut child = Command::new(program).args(args).spawn()?;
    // SAFETY: both handles are valid for the duration of the call.
    if unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as _) } == 0 {
        let err = std::io::Error::last_os_error();
        let _ = child.kill();
        return Err(err);
    }
    let status = child.wait()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::resolve_in;
    use std::ffi::OsStr;

    #[test]
    fn finds_an_npm_cmd_shim_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex.cmd"), b"").unwrap();
        let path = dir.path().as_os_str();
        let found = resolve_in(OsStr::new("codex"), path, OsStr::new(".EXE;.CMD"));
        let found = found.expect("the shim is found");
        let name = found.file_name().unwrap().to_string_lossy();
        assert!(name.eq_ignore_ascii_case("codex.cmd"), "{name}");
    }

    #[test]
    fn a_program_with_a_directory_is_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex.cmd"), b"").unwrap();
        assert_eq!(
            resolve_in(OsStr::new(r".\codex"), dir.path().as_os_str(), OsStr::new(".CMD")),
            None
        );
    }
}
