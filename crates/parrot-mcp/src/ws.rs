//! The upstream connection over WebSocket (spec 3.4), bridged to a line stream so the client
//! code is the same as over the local socket.

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// Connects to `url` with the client token as the second subprotocol, and returns one end of a
/// duplex stream that carries one JSON message per line.
pub async fn connect(url: &str, token: Option<&str>) -> std::io::Result<DuplexStream> {
    let mut request = url.into_client_request().map_err(std::io::Error::other)?;
    let protocols = match token {
        Some(t) => format!("parrot.1, parrot.token.{t}"),
        None => "parrot.1".into(),
    };
    let value = HeaderValue::from_str(&protocols).map_err(std::io::Error::other)?;
    request.headers_mut().insert("Sec-WebSocket-Protocol", value);
    let (socket, _) = tokio_tungstenite::connect_async(request)
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
