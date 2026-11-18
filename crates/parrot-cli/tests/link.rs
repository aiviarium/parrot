//! Links through the real `parrot connect --link`: the `command` carrier runs it as a relay to a
//! test link endpoint, and the Noise session runs end to end through it.
#![cfg(unix)]

use parrot_link::carrier::{CarrierRef, Carriers};
use parrot_link::endpoint::{accept_same_user, bind_link_socket, link_socket_path};
use parrot_link::keys::PinSet;
use parrot_link::noise::{Dialer, Listener};
use parrot_link::pairing::InviteBook;
use parrot_link::{LinkError, StaticKey};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PARROT: &str = env!("CARGO_BIN_EXE_parrot");

struct Endpoint {
    _dir: tempfile::TempDir,
    sock: PathBuf,
    key: Arc<StaticKey>,
    pins: Arc<PinSet>,
    invites: Arc<InviteBook>,
}

/// A link endpoint like the runtime's: `link.sock` beside `sock`, answering each message with
/// `{"echo": …}`.
fn endpoint() -> Endpoint {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("run/sock");
    let listener = bind_link_socket(&link_socket_path(&sock)).unwrap();
    let key = Arc::new(StaticKey::from_private([0xb1; 32]));
    let pins = Arc::new(PinSet::new());
    let invites = Arc::new(InviteBook::new());
    let link = Arc::new(Listener::new(key.clone(), pins.clone(), invites.clone()));
    tokio::spawn(async move {
        loop {
            let stream = accept_same_user(&listener).await.unwrap();
            let link = link.clone();
            tokio::spawn(async move {
                let Ok(mut conn) = link.accept(stream).await else {
                    return;
                };
                while let Ok(Some(msg)) = conn.recv_json().await {
                    let reply = json!({ "echo": msg, "from": conn.peer.runtime });
                    if conn.send_json(&reply).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    Endpoint {
        _dir: dir,
        sock,
        key,
        pins,
        invites,
    }
}

fn relay(sock: &Path) -> CarrierRef {
    serde_json::from_value(json!({
        "kind": "command",
        "target": { "argv": [PARROT, "connect", "--link", "--socket", sock] },
    }))
    .unwrap()
}

#[tokio::test]
async fn command_carrier_through_connect_link() {
    let ep = endpoint();
    let ours = Arc::new(StaticKey::from_private([0xa1; 32]));
    ep.pins.pin(ours.public());
    let channel = Carriers::new().open(&relay(&ep.sock)).await.unwrap();
    assert_eq!(channel.kind, "command");
    let mut conn = Dialer::new(ours.clone())
        .dial(channel.stream, &ep.key.public())
        .await
        .unwrap();
    assert_eq!(conn.peer.runtime, ep.key.runtime_id());
    let big = "y".repeat(200_000);
    conn.send_json(&json!({ "n": 1, "big": big })).await.unwrap();
    let reply = conn.recv_json().await.unwrap().unwrap();
    assert_eq!(reply["echo"]["big"].as_str().map(str::len), Some(200_000));
    assert_eq!(reply["from"], ours.runtime_id());
}

#[tokio::test]
async fn pairing_through_connect_link() {
    let ep = endpoint();
    let ours = Arc::new(StaticKey::from_private([0xa2; 32]));
    let code = ep.invites.create(ep.key.public(), vec![], 60_000, now_ms());
    let parsed = parrot_link::pairing::InviteCode::parse(&code.encode()).unwrap();
    let channel = Carriers::new().open(&relay(&ep.sock)).await.unwrap();
    let mut conn = Dialer::new(ours.clone())
        .pair(channel.stream, &parsed)
        .await
        .unwrap();
    assert_eq!(conn.paired.as_deref(), Some(code.invite.as_str()));
    conn.send_json(&json!("hello")).await.unwrap();
    assert_eq!(conn.recv_json().await.unwrap().unwrap()["echo"], "hello");
}

#[tokio::test]
async fn unpinned_key_gets_nothing_through_the_relay() {
    let ep = endpoint();
    let stranger = Arc::new(StaticKey::from_private([0xc1; 32]));
    let channel = Carriers::new().open(&relay(&ep.sock)).await.unwrap();
    // The dialler's side finishes; the endpoint refuses its key and closes.
    let mut conn = Dialer::new(stranger)
        .dial(channel.stream, &ep.key.public())
        .await
        .unwrap();
    let _ = conn.send_json(&json!("anyone?")).await;
    let got = tokio::time::timeout(Duration::from_secs(10), conn.recv_json())
        .await
        .unwrap();
    assert!(matches!(got, Ok(None) | Err(_)), "{got:?}");
}

#[tokio::test]
async fn wrong_endpoint_key_is_refused() {
    let ep = endpoint();
    let ours = Arc::new(StaticKey::from_private([0xa3; 32]));
    ep.pins.pin(ours.public());
    let channel = Carriers::new().open(&relay(&ep.sock)).await.unwrap();
    let expected = StaticKey::from_private([0xd1; 32]).public();
    let err = Dialer::new(ours)
        .dial(channel.stream, &expected)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, LinkError::Key { .. }), "{err}");
}

#[test]
fn connect_link_refuses_a_shared_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let run = dir.path().join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(run.join("link.sock")).unwrap();
    let out = Command::new(PARROT)
        .args(["connect", "--link", "--socket"])
        .arg(run.join("sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("0700"), "{err}");
}

#[test]
fn add_reads_the_code_from_stdin_and_checks_it() {
    let mut child = Command::new(PARROT)
        .args([
            "link",
            "add",
            "--name",
            "lab",
            "--tcp",
            "192.0.2.10:7790",
            "--code-stdin",
        ])
        .args(["--socket", "/nonexistent/sock"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    writeln!(child.stdin.take().unwrap(), "not-a-code").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a pairing code"), "{err}");
}

#[test]
fn link_list_reports_an_unsupported_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run/sock");
    let mut serve = Command::new(PARROT)
        .args(["serve", "--state-dir"])
        .arg(dir.path().join("state"))
        .env("PARROT_SOCKET", &socket)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "serve never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = Command::new(PARROT)
        .args(["link", "list"])
        .env("PARROT_SOCKET", &socket)
        .output()
        .unwrap();
    serve.kill().unwrap();
    serve.wait().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Until the runtime implements links it answers that it doesn't; afterwards, a list.
    if out.status.success() {
        let v: Value = serde_json::from_str(&stdout).unwrap();
        assert!(v["links"].is_array(), "{stdout}");
    } else {
        assert!(stderr.contains("does not support link/list"), "{stderr}");
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
