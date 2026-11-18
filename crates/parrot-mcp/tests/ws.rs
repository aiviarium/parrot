//! The upstream connection over `wss://` to a local server with a made-up certificate.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;

// The made-up certificate for 127.0.0.1 that the push test in parrot-serve also uses.
const CERT: &[u8] = include_bytes!("../../parrot-serve/fixtures/tls/push-test.crt");
const KEY: &[u8] = include_bytes!("../../parrot-serve/fixtures/tls/push-test.key");

/// A `wss://` server that answers each text message with the same text, and sends the
/// subprotocols the client asked for on the channel.
async fn echo_server() -> (u16, tokio::sync::mpsc::UnboundedReceiver<String>) {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let config =
        rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(CERT).unwrap()],
                PrivateKeyDer::from_pem_slice(KEY).unwrap(),
            )
            .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                // The error type is tungstenite's, fixed by `accept_hdr_async`.
                #[allow(clippy::result_large_err)]
                let callback = |req: &Request, mut resp: Response| {
                    let asked = req
                        .headers()
                        .get("Sec-WebSocket-Protocol")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let _ = tx.send(asked);
                    resp.headers_mut()
                        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("parrot.1"));
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tls, callback).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    if let Message::Text(t) = msg
                        && ws.send(Message::text(t.to_string())).await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    (port, rx)
}

#[tokio::test]
async fn wss_with_a_trusted_root_carries_lines() {
    let (port, mut asked) = echo_server().await;
    let url = format!("wss://127.0.0.1:{port}/parrot");
    let stream = parrot_mcp::ws::connect(&url, Some("tok-made-up"), Some(CERT))
        .await
        .expect("a wss connection");
    let protocols = asked.recv().await.unwrap();
    assert_eq!(protocols, "parrot.1, parrot.token.tok-made-up");
    let (r, mut w) = tokio::io::split(stream);
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(r).lines();
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("an echo in time")
        .unwrap()
        .unwrap();
    assert_eq!(line, "{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}");
}

#[tokio::test]
async fn wss_refuses_a_certificate_outside_the_public_roots() {
    let (port, _) = echo_server().await;
    let url = format!("wss://127.0.0.1:{port}/parrot");
    let err = parrot_mcp::ws::connect(&url, None, None).await.unwrap_err();
    assert!(err.to_string().to_lowercase().contains("certificate"), "{err}");
}

#[test]
fn roots_need_a_certificate() {
    assert!(parrot_mcp::ws::tls(Some(b"no pem here")).is_err());
    assert!(parrot_mcp::ws::tls(None).is_ok());
}
