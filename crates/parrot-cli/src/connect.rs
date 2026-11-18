//! `parrot connect`: relays stdin and stdout to the local runtime's socket (spec 3.2).

use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

pub async fn run(socket: Option<PathBuf>, link: bool) -> Result<(), String> {
    if link {
        return Err("connect --link is not supported yet".into());
    }
    let path = match socket {
        Some(path) => path,
        None => crate::serve::socket_path().map_err(|e| e.to_string())?,
    };
    let stream = open(&path)
        .await
        .map_err(|e| format!("cannot connect to {}: {e}", path.display()))?;
    relay(stream).await.map_err(|e| e.to_string())
}

#[cfg(unix)]
async fn open(path: &std::path::Path) -> std::io::Result<tokio::net::UnixStream> {
    tokio::net::UnixStream::connect(path).await
}

#[cfg(windows)]
async fn open(path: &std::path::Path) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    tokio::net::windows::named_pipe::ClientOptions::new().open(path)
}

/// Copies stdin to the stream and the stream to stdout. Ends when the runtime closes the stream;
/// the end of stdin only closes the writing half.
async fn relay<S: AsyncRead + AsyncWrite>(stream: S) -> std::io::Result<()> {
    let (mut r, mut w) = tokio::io::split(stream);
    let upstream = async {
        tokio::io::copy(&mut tokio::io::stdin(), &mut w).await?;
        w.shutdown().await
    };
    let downstream = async {
        let mut out = tokio::io::stdout();
        tokio::io::copy(&mut r, &mut out).await?;
        out.flush().await
    };
    tokio::pin!(downstream);
    tokio::select! {
        done = &mut downstream => done,
        up = upstream => {
            up?;
            downstream.await
        }
    }
}
