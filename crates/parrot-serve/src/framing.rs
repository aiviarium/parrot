//! Line framing and message classification shared by the transports (spec 3.1).

use crate::dispatch::Runtime;
use parrot_core::{ConnId, Input, Peer};
use parrot_proto::rpc::{Response, RpcError};
use serde_json::Value;
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// The largest message a connection accepts; a longer one closes it.
pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;

/// Reads one line into `buf` without its `\n` or `\r\n`. Returns `false` at end of input. A line
/// longer than `limit` fails with `InvalidData` before more than `limit` bytes are buffered.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
    limit: usize,
) -> io::Result<bool> {
    buf.clear();
    loop {
        let avail = r.fill_buf().await?;
        if avail.is_empty() {
            return Ok(!buf.is_empty());
        }
        if let Some(i) = avail.iter().position(|&b| b == b'\n') {
            if buf.len() + i > limit {
                return Err(too_long());
            }
            buf.extend_from_slice(&avail[..i]);
            r.consume(i + 1);
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(true);
        }
        let n = avail.len();
        if buf.len() + n > limit {
            return Err(too_long());
        }
        buf.extend_from_slice(avail);
        r.consume(n);
    }
}

fn too_long() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "message over the size limit")
}

/// What one incoming message means for the runtime.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// Feed this to the core.
    Input(Input),
    /// Answer the sender with this line; the core never sees the message.
    Reject(String),
    /// Nothing to do, such as a response to a call the runtime never made.
    Ignore,
}

/// Classifies one message from a client: requests and notifications become `Input::Call`,
/// responses become `Input::ClientReply`, and batches and malformed messages are rejected.
pub fn classify(conn: &ConnId, text: &str) -> Incoming {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return reject(Value::Null, -32700, format!("parse error: {e}")),
    };
    let mut obj = match value {
        Value::Object(o) => o,
        Value::Array(_) => return reject(Value::Null, -32600, "batches are not supported"),
        _ => return reject(Value::Null, -32600, "a message is a JSON object"),
    };
    let id = obj.remove("id");
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return reject(id.unwrap_or(Value::Null), -32600, "jsonrpc must be \"2.0\"");
    }
    match obj.remove("method") {
        Some(Value::String(method)) => Incoming::Input(Input::Call {
            conn: conn.clone(),
            id,
            method,
            params: obj.remove("params").unwrap_or(Value::Null),
        }),
        Some(_) => reject(id.unwrap_or(Value::Null), -32600, "method must be a string"),
        None => {
            // A response; it answers an `Effect::ClientCall`, whose ids are numbers.
            let Some(call) = id.as_ref().and_then(Value::as_u64) else {
                return Incoming::Ignore;
            };
            let result = if let Some(error) = obj.remove("error") {
                Err(serde_json::from_value::<RpcError>(error)
                    .unwrap_or_else(|_| RpcError::new(-32603, "malformed error from client")))
            } else if let Some(result) = obj.remove("result") {
                Ok(result)
            } else {
                return Incoming::Ignore;
            };
            Incoming::Input(Input::ClientReply {
                conn: conn.clone(),
                call,
                result,
            })
        }
    }
}

fn reject(id: Value, code: i64, message: impl Into<String>) -> Incoming {
    let response = Response::err(id, RpcError::new(code, message));
    Incoming::Reject(serde_json::to_string(&response).expect("a response serializes"))
}

/// Serves one line-framed connection (stdio, local socket, named pipe) until either side ends it.
pub async fn serve_lines<R, W>(rt: Runtime, peer: Peer, reader: R, mut writer: W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut conn = rt.connect(peer).await?;
    let id = conn.id.clone();
    let mut reader = BufReader::new(reader);

    let read = async {
        let mut buf = Vec::new();
        loop {
            match read_line(&mut reader, &mut buf, MAX_MESSAGE).await {
                Ok(true) if buf.iter().all(u8::is_ascii_whitespace) => {}
                Ok(true) => {
                    let incoming = match std::str::from_utf8(&buf) {
                        Ok(text) => classify(&id, text),
                        Err(_) => reject(Value::Null, -32700, "parse error: not UTF-8"),
                    };
                    if !rt.incoming(&id, incoming).await {
                        return false;
                    }
                }
                // End of input: keep writing what the core still answers.
                Ok(false) => return true,
                Err(_) => return false,
            }
        }
    };
    let rx = &mut conn.rx;
    let kill = conn.kill.clone();
    let mut write_ok = true;
    let mut drain = false;
    {
        let write = async {
            while let Some(line) = rx.recv().await {
                if write_line(&mut writer, &line).await.is_err() {
                    return;
                }
            }
        };
        tokio::select! {
            eof = read => drain = eof,
            _ = write => write_ok = false,
            _ = kill.notified() => {}
        }
    }
    rt.disconnect(&id).await;
    if drain && write_ok {
        // The actor drops this connection's queue after `Disconnected`, which ends the loop.
        while let Some(line) = conn.rx.recv().await {
            if write_line(&mut writer, &line).await.is_err() {
                break;
            }
        }
    }
    let _ = writer.shutdown().await;
    Ok(())
}

async fn write_line<W: AsyncWrite + Unpin>(w: &mut W, line: &str) -> io::Result<()> {
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_lf_and_crlf_lines() {
        let mut r = BufReader::new(&b"a\r\nbb\nlast"[..]);
        let mut buf = Vec::new();
        let mut lines = Vec::new();
        while read_line(&mut r, &mut buf, 100).await.unwrap() {
            lines.push(String::from_utf8(buf.clone()).unwrap());
        }
        assert_eq!(lines, ["a", "bb", "last"]);
    }

    #[tokio::test]
    async fn long_line_fails() {
        let mut r = BufReader::new(&b"0123456789\n"[..]);
        let mut buf = Vec::new();
        assert!(read_line(&mut r, &mut buf, 5).await.is_err());
    }

    #[test]
    fn classifies_messages() {
        let conn = "con_x".to_string();
        assert!(matches!(
            classify(&conn, r#"[{"jsonrpc":"2.0","id":1,"method":"a"}]"#),
            Incoming::Reject(l) if l.contains("-32600")
        ));
        assert!(matches!(classify(&conn, "{"), Incoming::Reject(l) if l.contains("-32700")));
        assert_eq!(
            classify(&conn, r#"{"jsonrpc":"2.0","method":"n"}"#),
            Incoming::Input(Input::Call {
                conn: conn.clone(),
                id: None,
                method: "n".into(),
                params: Value::Null
            })
        );
        assert_eq!(
            classify(&conn, r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#),
            Incoming::Input(Input::ClientReply {
                conn: conn.clone(),
                call: 7,
                result: Ok(serde_json::json!({"ok": true}))
            })
        );
        assert_eq!(
            classify(&conn, r#"{"jsonrpc":"2.0","id":"x","result":1}"#),
            Incoming::Ignore
        );
    }
}
