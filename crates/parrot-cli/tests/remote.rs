//! The `remote` profile (spec 3.4, 36, E8): a request with `notify` reaches a webhook, and a
//! WebSocket client whose token has only `answer` answers it from the push.
#![cfg(unix)]
// Unix only for now: these runtime tests have not run on Windows yet.

mod common;

use common::{Client, Serve, Setup, WAIT};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// A webhook test server: each POST body, parsed as JSON, goes to the channel.
async fn webhook() -> (u16, mpsc::UnboundedReceiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() < end + 4 + len {
                        continue;
                    }
                    let body = &buf[end + 4..end + 4 + len];
                    let _ = s
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    if let Ok(v) = serde_json::from_slice(body) {
                        let _ = tx.send(v);
                    }
                    return;
                }
            });
        }
    });
    (port, rx)
}

/// A WebSocket client with a token, as a [`Client`].
async fn connect_phone(port: u16, secret: &str) -> Result<Client, String> {
    let mut req = format!("ws://127.0.0.1:{port}/parrot")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        format!("parrot.1, parrot.token.{secret}").parse().unwrap(),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| e.to_string())?;
    let (mut sink, mut source) = ws.split();
    let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (msgs, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if sink.send(Message::Text(line.trim_end().into())).await.is_err() {
                return;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(Message::Text(t))) = source.next().await {
            if let Ok(v) = serde_json::from_str::<Value>(t.as_str())
                && msgs.send(v).is_err()
            {
                return;
            }
        }
    });
    Ok(Client::from_channels("phone", out, rx))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_answers_a_request_from_a_push() {
    let (hook, mut pushes) = webhook().await;
    let ws = common::free_port();
    let serve = Serve::start(Setup {
        config: Some(json!({ "notify": [format!("webhook:http://127.0.0.1:{hook}/hook")] })),
        env: common::fake_claude_env(&common::scenario("bash-allowed.jsonl")),
        args: vec!["--ws".into(), format!("127.0.0.1:{ws}")],
    });
    let mut owner = Client::connect(&serve, "owner").await;
    owner.initialize().await;
    let work = serve.work_dir();
    let created = owner
        .ok(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": work.to_string_lossy() } }),
        )
        .await;
    let session = created["session"]["id"].as_str().unwrap().to_string();

    let made = owner
        .ok(
            "token/create",
            json!({ "name": "phone", "scopes": ["answer"], "sessions": [session] }),
        )
        .await;
    let secret = made["secret"].as_str().unwrap().to_string();
    let listed = owner.ok("token/list", json!({})).await;
    assert_eq!(listed["tokens"][0]["name"], "phone", "{listed}");
    assert!(
        !listed.to_string().contains(&secret),
        "token/list shows the secret"
    );
    let file = std::fs::read_to_string(serve.state_dir().join("tokens.json")).unwrap();
    assert!(!file.contains(&secret), "the secret is stored in the clear");

    owner
        .ok("session/prompt", json!({ "session": session, "text": "run ls" }))
        .await;
    let push = tokio::time::timeout(WAIT, pushes.recv())
        .await
        .expect("a push in time")
        .unwrap();
    assert_eq!(push["event"], "request", "{push}");
    assert_eq!(push["session"], session.as_str(), "{push}");
    let request = push["request"].clone();
    assert!(request.is_string(), "{push}");

    let deadline = std::time::Instant::now() + WAIT;
    let mut phone = loop {
        match connect_phone(ws, &secret).await {
            Ok(p) => break p,
            Err(e) => {
                assert!(std::time::Instant::now() < deadline, "no websocket: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    };
    let init = phone.initialize().await;
    assert_eq!(init["connection"]["scopes"], json!(["answer"]), "{init}");
    assert_eq!(init["connection"]["sessions"], json!([session]), "{init}");

    // `answer` alone: no reading, prompting or making roots.
    let read = phone.call("session/list", json!({})).await;
    assert_eq!(read["error"]["code"], -32003, "{read}");
    let root = phone
        .call(
            "session/new",
            json!({ "agent": "claude", "workspace": { "kind": "dir", "path": "/" } }),
        )
        .await;
    assert_eq!(root["error"]["code"], -32003, "{root}");

    phone
        .ok(
            "request/answer",
            json!({ "request": request, "answer": { "choice": "allow" } }),
        )
        .await;
    owner
        .wait_update(0, "the turn's end", |u| {
            u["type"] == "turn" && u["turn"]["status"] == "completed"
        })
        .await;

    // A revoked token's connections close, and it no longer opens one.
    owner
        .ok("token/revoke", json!({ "token": made["token"]["id"] }))
        .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match tokio::time::timeout_at(deadline, phone.rx.recv()).await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => panic!("the phone's connection stayed open"),
        }
    }
    assert!(connect_phone(ws, &secret).await.is_err());
}
