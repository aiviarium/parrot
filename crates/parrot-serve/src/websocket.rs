//! The WebSocket transport (spec 3.4): path `/parrot`, subprotocol `parrot.1`, and the client
//! token as a second subprotocol `parrot.token.<token>`. One message per text frame.

use crate::dispatch::Runtime;
use crate::framing::{Incoming, MAX_MESSAGE, classify};
use crate::tokens::TokenCheck;
use futures::{SinkExt, StreamExt};
use parrot_core::Peer;
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

pub const PATH: &str = "/parrot";
pub const SUBPROTOCOL: &str = "parrot.1";
pub const TOKEN_PREFIX: &str = "parrot.token.";

/// Accepts WebSocket clients forever. Without TLS the listener must be on loopback; serve a TLS
/// stream with `serve_websocket_stream` otherwise.
pub async fn serve_websocket(
    rt: Runtime,
    listener: TcpListener,
    tokens: Arc<dyn TokenCheck>,
) -> io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a WebSocket listener off loopback needs TLS",
        ));
    }
    loop {
        let (stream, _) = listener.accept().await?;
        let rt = rt.clone();
        let tokens = tokens.clone();
        tokio::spawn(async move {
            let _ = serve_websocket_stream(rt, stream, tokens).await;
        });
    }
}

#[allow(clippy::result_large_err)] // the handshake callback type is fixed by tungstenite
/// Upgrades one stream, checks its token and serves it until either side closes.
pub async fn serve_websocket_stream<S>(rt: Runtime, stream: S, tokens: Arc<dyn TokenCheck>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut peer = None;
    let callback = |req: &Request, mut resp: Response| -> Result<Response, ErrorResponse> {
        if req.uri().path() != PATH {
            return Err(refuse(StatusCode::NOT_FOUND, "not found"));
        }
        let offered: Vec<&str> = req
            .headers()
            .get_all("sec-websocket-protocol")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .collect();
        if !offered.contains(&SUBPROTOCOL) {
            return Err(refuse(
                StatusCode::BAD_REQUEST,
                "subprotocol parrot.1 is required",
            ));
        }
        let checked = offered
            .iter()
            .find_map(|p| p.strip_prefix(TOKEN_PREFIX))
            .and_then(|token| tokens.check(token));
        let Some(p) = checked else {
            return Err(refuse(
                StatusCode::UNAUTHORIZED,
                "a valid client token is required",
            ));
        };
        peer = Some(p);
        resp.headers_mut()
            .insert("sec-websocket-protocol", HeaderValue::from_static(SUBPROTOCOL));
        Ok(resp)
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let ws = tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(config))
        .await
        .map_err(io::Error::other)?;
    let peer: Peer = peer.expect("the callback accepted a token");

    let mut conn = rt.connect(peer).await?;
    let id = conn.id.clone();
    let (mut sink, mut source) = ws.split();
    let read = async {
        while let Some(msg) = source.next().await {
            let text = match msg {
                Ok(Message::Text(text)) => text,
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                // Binary frames are not part of the protocol; close frames and errors end it.
                _ => return,
            };
            let incoming: Incoming = classify(&id, text.as_str());
            if !rt.incoming(&id, incoming).await {
                return;
            }
        }
    };
    let rx = &mut conn.rx;
    let write = async {
        while let Some(line) = rx.recv().await {
            if sink.send(Message::Text(line.into())).await.is_err() {
                return;
            }
        }
    };
    let kill = conn.kill.clone();
    // The write loop ends after the runtime closes the connection and its queue is sent.
    tokio::select! {
        _ = read => {}
        _ = write => {}
        _ = kill.notified() => {}
    }
    rt.disconnect(&id).await;
    let _ = sink.close().await;
    Ok(())
}

fn refuse(status: StatusCode, body: &str) -> ErrorResponse {
    let mut resp = ErrorResponse::new(Some(body.to_string()));
    *resp.status_mut() = status;
    resp
}
