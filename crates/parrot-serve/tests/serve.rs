//! The local socket and WebSocket transports. Both tests drive a Unix socket, so they are Unix only.
#![cfg(unix)]

use futures::{SinkExt, StreamExt};
use parrot_core::clock::{ManualClock, SeqIds};
use parrot_core::{Core, Peer};
use parrot_serve::tokens::StaticTokens;
use parrot_serve::{NoopBackend, Runtime, socket, websocket};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn runtime() -> Runtime {
    let clock = ManualClock::default();
    let core = Core::new(Box::new(clock.clone()), Box::new(SeqIds::default()));
    Runtime::spawn(core, Box::new(clock), |_| NoopBackend).0
}

async fn with_socket() -> (Runtime, tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parrot/sock");
    // A leftover file from an earlier run counts as stale.
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"").unwrap();
    let listener = socket::bind_socket(&path).unwrap();
    let rt = runtime();
    tokio::spawn(socket::serve_socket(rt.clone(), listener));
    (rt, dir, path)
}

async fn wait_for_conns(rt: &Runtime, n: usize) -> Vec<(String, Peer)> {
    for _ in 0..100 {
        let conns = rt.connections().await.unwrap();
        if conns.len() == n {
            return conns;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} connections");
}

#[tokio::test]
async fn unknown_method_gets_method_not_found() {
    let (_rt, dir, path) = with_socket().await;
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("parrot"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    let stream = UnixStream::connect(&path).await.unwrap();
    let (r, mut w) = stream.into_split();
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"nope\"}\r\n[]\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(r).lines();
    let reply: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(reply["id"], json!(1));
    assert_eq!(reply["error"]["code"], json!(-32601));
    let batch: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(batch["error"]["code"], json!(-32600));
}

#[tokio::test]
async fn replies_queued_before_eof_are_written() {
    let (_rt, _dir, path) = with_socket().await;
    let mut stream = UnixStream::connect(&path).await.unwrap();
    stream
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"a\",\"method\":\"nope\"}\n")
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert!(out.contains("-32601"), "{out}");
}

#[tokio::test]
async fn oversized_line_closes_the_connection() {
    let (rt, _dir, path) = with_socket().await;
    let mut stream = UnixStream::connect(&path).await.unwrap();
    wait_for_conns(&rt, 1).await;
    let chunk = vec![b'x'; 1024 * 1024];
    // The runtime may close mid-write; that is the point.
    for _ in 0..33 {
        if stream.write_all(&chunk).await.is_err() {
            break;
        }
    }
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
    wait_for_conns(&rt, 0).await;
}

#[tokio::test]
async fn connections_get_distinct_ids() {
    let (rt, _dir, path) = with_socket().await;
    let _a = UnixStream::connect(&path).await.unwrap();
    let _b = UnixStream::connect(&path).await.unwrap();
    let conns = wait_for_conns(&rt, 2).await;
    assert_ne!(conns[0].0, conns[1].0);
    assert!(
        conns
            .iter()
            .all(|(id, peer)| id.starts_with("con_") && *peer == Peer::Owner)
    );
}

#[tokio::test]
async fn second_runtime_refuses_a_live_socket() {
    let (_rt, _dir, path) = with_socket().await;
    let err = socket::bind_socket(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
}

#[tokio::test]
async fn websocket_with_token() {
    let rt = runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let peer = Peer::Token {
        token: "tok_test".into(),
        scopes: vec!["read".into()],
        sessions: None,
        provides: None,
    };
    let tokens = StaticTokens([("s3cret".to_string(), peer.clone())].into());
    tokio::spawn(websocket::serve_websocket(rt.clone(), listener, Arc::new(tokens)));

    let request = |proto: &str| {
        let mut req = format!("ws://{addr}/parrot").into_client_request().unwrap();
        req.headers_mut()
            .insert("sec-websocket-protocol", proto.parse().unwrap());
        req
    };
    assert!(
        tokio_tungstenite::connect_async(request("parrot.1, parrot.token.wrong"))
            .await
            .is_err()
    );

    let (mut ws, resp) = tokio_tungstenite::connect_async(request("parrot.1, parrot.token.s3cret"))
        .await
        .unwrap();
    assert_eq!(resp.headers()["sec-websocket-protocol"], "parrot.1");
    assert_eq!(wait_for_conns(&rt, 1).await[0].1, peer);
    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":2,"method":"nope"}"#.into(),
    ))
    .await
    .unwrap();
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        panic!("expected a text frame");
    };
    let reply: Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(reply["id"], json!(2));
    assert_eq!(reply["error"]["code"], json!(-32601));
    ws.close(None).await.unwrap();
    wait_for_conns(&rt, 0).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn broker_authenticates_the_unix_peer() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broker.sock");
    let local_uid = std::fs::metadata(dir.path()).unwrap().uid();
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let rt = runtime();
    let server = tokio::spawn(socket::serve_broker(rt.clone(), listener));
    let stream = UnixStream::connect(path).await.unwrap();

    if local_uid == 0 {
        let conns = wait_for_conns(&rt, 1).await;
        let Peer::Token { scopes, .. } = &conns[0].1 else {
            panic!("the broker does not get owner privileges");
        };
        assert!(scopes.iter().any(|scope| scope == "answer"));
        assert!(!scopes.iter().any(|scope| scope == "functions"));
    } else {
        // A permissive file mode on the listener still grants no protocol access.
        let mut output = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(stream).read_to_string(&mut output),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(output.is_empty());
        assert!(rt.connections().await.unwrap().is_empty());
    }
    server.abort();
}
