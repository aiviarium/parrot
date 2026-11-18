//! The upstream connection over WebSocket (spec 3.4), bridged to a line stream so the client
//! code is the same as over the local socket. `wss://` is checked against the public roots, or
//! against the certificates given instead, as for a runtime with its own certificate.

use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The TLS settings for `wss://`: the certificates in `roots` (PEM) when given, else the public
/// roots.
pub fn tls(roots: Option<&[u8]>) -> std::io::Result<Arc<rustls::ClientConfig>> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    let mut store = rustls::RootCertStore::empty();
    match roots {
        Some(pem) => {
            for cert in CertificateDer::pem_slice_iter(pem) {
                store
                    .add(cert.map_err(std::io::Error::other)?)
                    .map_err(std::io::Error::other)?;
            }
            if store.is_empty() {
                return Err(std::io::Error::other("the file holds no certificate"));
            }
        }
        None => store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    let config =
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(std::io::Error::other)?
            .with_root_certificates(store)
            .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Connects to `url` with the client token as the second subprotocol, and returns one end of a
/// duplex stream that carries one JSON message per line. `roots` replaces the public roots for
/// `wss://`.
pub async fn connect(url: &str, token: Option<&str>, roots: Option<&[u8]>) -> std::io::Result<DuplexStream> {
    let mut request = url.into_client_request().map_err(std::io::Error::other)?;
    let protocols = match token {
        Some(t) => format!("parrot.1, parrot.token.{t}"),
        None => "parrot.1".into(),
    };
    let value = HeaderValue::from_str(&protocols).map_err(std::io::Error::other)?;
    request.headers_mut().insert("Sec-WebSocket-Protocol", value);
    let connector = Connector::Rustls(tls(roots)?);
    let (socket, _) = tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .map_err(std::io::Error::other)?;
    let (mut sink, mut stream) = socket.split();
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let (r, mut w) = tokio::io::split(theirs);
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if sink.send(Message::text(line)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => break,
                _ => continue,
            };
            if w.write_all(text.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });
    Ok(ours)
}
