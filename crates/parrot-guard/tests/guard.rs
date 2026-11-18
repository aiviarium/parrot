#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn guard_exe() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    deps.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join("guard")
}

fn alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // A zombie has already gone; it only waits to be reaped.
        Ok(stat) => !stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z'),
        Err(_) => false,
    }
}

fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.ends_with('\n')
        {
            return text;
        }
        assert!(Instant::now() < deadline, "no pid file");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_gone(pid: i32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !alive(pid)
}

#[test]
fn grandchild_dies_with_the_guards_parent() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    // parent (sh) -> guard -> sh -> sleep; the grandchild writes its pid.
    let inner = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
    let mut parent = Command::new("sh")
        .arg("-c")
        .arg(r#""$0" -- sh -c "$1" & wait"#)
        .arg(guard_exe())
        .arg(inner)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let grandchild: i32 = wait_for_file(&pidfile).trim().parse().unwrap();
    assert!(alive(grandchild));

    parent.kill().unwrap();
    parent.wait().unwrap();
    assert!(
        wait_gone(grandchild, Duration::from_secs(1)),
        "grandchild {grandchild} outlived the guard's parent"
    );
}

#[test]
fn exit_status_passes_through() {
    let status = Command::new(guard_exe())
        .args(["--", "sh", "-c", "exit 7"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(7));
}

#[test]
fn sigterm_is_forwarded_to_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let inner = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
    let mut guard = Command::new(guard_exe())
        .args(["--", "sh", "-c", &inner])
        .spawn()
        .unwrap();
    let grandchild: i32 = wait_for_file(&pidfile).trim().parse().unwrap();
    // SAFETY: kill has no memory effects.
    unsafe { libc::kill(guard.id() as i32, libc::SIGTERM) };
    let status = guard.wait().unwrap();
    assert_eq!(status.code(), Some(128 + libc::SIGTERM));
    assert!(wait_gone(grandchild, Duration::from_secs(1)));
}
