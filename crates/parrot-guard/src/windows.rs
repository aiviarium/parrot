//! The guard on Windows: a job object that kills the child tree when the guard's handle closes.

use std::ffi::OsString;
use std::os::windows::io::AsRawHandle;
use std::process::Command;

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};

pub fn run(program: OsString, args: Vec<OsString>) -> std::io::Result<i32> {
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
