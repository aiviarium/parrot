//! The guard on Linux and macOS: a process group, a parent watch and signal forwarding.

use std::ffi::OsString;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(100);

enum Event {
    Signal(i32),
    ParentGone,
    Idle,
}

/// Sends `sig` to the process group `pgid`, ignoring a group that is already gone.
pub fn kill_group(pgid: i32, sig: i32) {
    if pgid > 0 {
        // SAFETY: killpg has no memory effects.
        unsafe { libc::killpg(pgid, sig) };
    }
}

fn signal_set() -> libc::sigset_t {
    // SAFETY: sigset_t is plain data, initialised by sigemptyset.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGCHLD] {
            libc::sigaddset(&mut set, sig);
        }
        set
    }
}

fn parent_pid() -> i32 {
    // SAFETY: getppid cannot fail.
    unsafe { libc::getppid() }
}

pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(libc::SIGKILL))
}

pub fn run(program: OsString, args: Vec<OsString>) -> std::io::Result<i32> {
    let parent = parent_pid();
    let set = signal_set();
    // The guard is single threaded, so blocking here blocks the signals for the whole process; they
    // are then taken synchronously by the watch.
    // SAFETY: set is a valid sigset_t.
    unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
    let watch = ParentWatch::new(parent, set)?;
    if parent_pid() != parent {
        // The parent died before the watch was armed.
        return Ok(128 + libc::SIGKILL);
    }

    let mut cmd = Command::new(program);
    cmd.args(args).process_group(0);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            let mut empty: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut empty);
            libc::pthread_sigmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let pgid = child.id() as i32;
    let mut kill_at: Option<Instant> = None;

    loop {
        if let Some(status) = child.try_wait()? {
            // Nothing in the group outlives its leader.
            kill_group(pgid, libc::SIGKILL);
            return Ok(exit_code(status));
        }
        if parent_pid() != parent {
            kill_group(pgid, libc::SIGKILL);
            let status = child.wait()?;
            return Ok(exit_code(status));
        }
        if kill_at.is_some_and(|at| Instant::now() >= at) {
            kill_group(pgid, libc::SIGKILL);
            kill_at = None;
        }
        match watch.wait(POLL) {
            Event::Signal(sig) if sig == libc::SIGTERM || sig == libc::SIGINT => {
                kill_group(pgid, sig);
                kill_at.get_or_insert(Instant::now() + crate::GRACE);
            }
            Event::ParentGone => {
                kill_group(pgid, libc::SIGKILL);
                let status = child.wait()?;
                return Ok(exit_code(status));
            }
            Event::Signal(_) | Event::Idle => {}
        }
    }
}

#[cfg(target_os = "linux")]
struct ParentWatch {
    parent: i32,
    set: libc::sigset_t,
}

#[cfg(target_os = "linux")]
impl ParentWatch {
    fn new(parent: i32, set: libc::sigset_t) -> std::io::Result<Self> {
        // The parent-death signal fires when the thread that forked us exits, so SIGHUP is only a
        // hint; the parent pid decides.
        // SAFETY: prctl with PR_SET_PDEATHSIG takes a signal number.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGHUP) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { parent, set })
    }

    fn wait(&self, timeout: Duration) -> Event {
        let ts = libc::timespec {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_nsec: timeout.subsec_nanos() as libc::c_long,
        };
        // SAFETY: valid set and timespec; info may be null.
        let sig = unsafe { libc::sigtimedwait(&self.set, std::ptr::null_mut(), &ts) };
        match sig {
            libc::SIGHUP if parent_pid() != self.parent => Event::ParentGone,
            sig if sig > 0 => Event::Signal(sig),
            _ => Event::Idle,
        }
    }
}

#[cfg(not(target_os = "linux"))]
struct ParentWatch {
    kq: i32,
}

#[cfg(not(target_os = "linux"))]
impl ParentWatch {
    fn new(parent: i32, _set: libc::sigset_t) -> std::io::Result<Self> {
        // SAFETY: kqueue takes no arguments.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut changes = vec![kevent(
            parent as libc::uintptr_t,
            libc::EVFILT_PROC,
            libc::NOTE_EXIT,
        )];
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGCHLD] {
            changes.push(kevent(sig as libc::uintptr_t, libc::EVFILT_SIGNAL, 0));
        }
        // SAFETY: changes is a valid array of kevents; no events are read here.
        let rc = unsafe {
            libc::kevent(
                kq,
                changes.as_ptr(),
                changes.len() as _,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            // ESRCH: the parent is already gone, which run() notices through getppid.
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err);
            }
        }
        Ok(Self { kq })
    }

    fn wait(&self, timeout: Duration) -> Event {
        let ts = libc::timespec {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_nsec: timeout.subsec_nanos() as libc::c_long,
        };
        let mut out = kevent(0, 0, 0);
        // SAFETY: out has room for one event.
        let n = unsafe { libc::kevent(self.kq, std::ptr::null(), 0, &mut out, 1, &ts) };
        if n <= 0 {
            return Event::Idle;
        }
        if out.filter == libc::EVFILT_PROC {
            Event::ParentGone
        } else {
            Event::Signal(out.ident as i32)
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn kevent(ident: libc::uintptr_t, filter: i16, fflags: u32) -> libc::kevent {
    // SAFETY: kevent is plain data.
    let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
    ev.ident = ident;
    ev.filter = filter;
    ev.flags = libc::EV_ADD;
    ev.fflags = fflags;
    ev
}
