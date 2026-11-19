//! A second `parrot serve` on a running runtime's socket (spec 3.3): it fails, says why, and
//! leaves the running runtime and both of its sockets alone.
#![cfg(unix)]
// Unix sockets: on Windows `serve_pipe` binds inside the pipe server and there is no file to unlink.

mod common;

use common::{Client, Serve, Setup};
use serde_json::json;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[tokio::test]
async fn a_second_serve_fails_and_leaves_the_running_socket_alone() {
    let serve = Serve::start(Setup::default());
    let mut client = Client::connect(&serve, "first").await;
    client.initialize().await;

    let mut second = Command::new(common::PARROT)
        .args(["serve", "--state-dir"])
        .arg(serve.state_dir())
        .env("PARROT_SOCKET", &serve.socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the second serve starts");
    let deadline = Instant::now() + common::WAIT;
    let status = loop {
        if let Some(status) = second.try_wait().expect("poll the second serve") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "a second serve kept running instead of failing"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let mut stderr = String::new();
    second
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!status.success(), "a second serve exited with {status}");
    assert!(stderr.contains("already listening"), "{stderr}");

    // The running runtime still answers, and its socket and link endpoint are still there.
    let sessions = client.ok("session/list", json!({})).await;
    assert!(sessions["sessions"].is_array(), "{sessions}");
    assert!(serve.socket.exists(), "the running runtime's socket is gone");
    assert!(
        serve.dir.path().join("link.sock").exists(),
        "the running runtime's link endpoint is gone"
    );
}
