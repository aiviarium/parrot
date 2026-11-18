//! `parrot connect`: relays stdin and stdout to the local runtime's socket (spec 3.2). With
//! `--link` it relays to the link endpoint `link.sock` beside the socket instead (E9); the bytes
//! are a Noise session that the runtime terminates, so the relay sees only ciphertext.

use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

pub async fn run(socket: Option<PathBuf>, link: bool, fd: Option<i32>) -> Result<(), String> {
    let path = match socket {
        Some(path) => path,
        None => crate::serve::socket_path().map_err(|e| e.to_string())?,
    };
    if let Some(fd) = fd {
        return run_link_fd(&path, fd).await;
    }
    if link {
        return run_link(&path).await;
    }
    let stream = open(&path)
        .await
        .map_err(|e| format!("cannot connect to {}: {e}", path.display()))?;
    exit_with(relay(stream).await)
}

/// Ends the process with the relay's outcome. Tokio reads stdin on a blocking thread that
/// shutdown waits for, so a caller that keeps our stdin open would keep a dead relay alive.
fn exit_with(result: std::io::Result<()>) -> ! {
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("parrot: {e}");
            std::process::exit(1)
        }
    }
}

async fn run_link(sock: &std::path::Path) -> Result<(), String> {
    let path = parrot_link::endpoint::link_socket_path(sock);
    let stream = parrot_link::endpoint::connect_link_socket(&path)
        .await
        .map_err(|e| format!("cannot connect to {}: {e}", path.display()))?;
    exit_with(relay(stream).await)
}

/// `connect --link --fd <n>`: relays an inherited, bidirectional descriptor, such as one end of
/// a socket a carrier egg accepted, to the link endpoint (spec 24.2).
#[cfg(unix)]
async fn run_link_fd(sock: &std::path::Path, fd: i32) -> Result<(), String> {
    use std::os::fd::{FromRawFd, OwnedFd};
    if fd < 3 {
        return Err(
            "--fd takes an inherited descriptor above 2; without it the relay uses stdin and stdout".into(),
        );
    }
    std::fs::metadata(format!("/dev/fd/{fd}")).map_err(|e| format!("descriptor {fd} is not open: {e}"))?;
    // SAFETY: the descriptor is open, as checked above, and the process that started this relay
    // handed it over for the relay to own and close.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let read = std::fs::File::from(owned);
    let write = read
        .try_clone()
        .map_err(|e| format!("cannot use descriptor {fd}: {e}"))?;
    let path = parrot_link::endpoint::link_socket_path(sock);
    let stream = parrot_link::endpoint::connect_link_socket(&path)
        .await
        .map_err(|e| format!("cannot connect to {}: {e}", path.display()))?;
    let outside = (tokio::fs::File::from_std(read), tokio::fs::File::from_std(write));
    exit_with(relay_between(outside, stream).await)
}

#[cfg(windows)]
async fn run_link_fd(_sock: &std::path::Path, _fd: i32) -> Result<(), String> {
    Err("connect --link --fd is not supported on Windows".into())
}

#[cfg(unix)]
async fn open(path: &std::path::Path) -> std::io::Result<tokio::net::UnixStream> {
    tokio::net::UnixStream::connect(path).await
}

#[cfg(windows)]
async fn open(path: &std::path::Path) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    parrot_link::pipe::connect_wait(path).await
}

/// Copies stdin to the stream and the stream to stdout. Ends when the runtime closes the stream;
/// the end of stdin only closes the writing half.
async fn relay<S: AsyncRead + AsyncWrite>(stream: S) -> std::io::Result<()> {
    relay_between((tokio::io::stdin(), tokio::io::stdout()), stream).await
}

/// Copies `input` to the stream and the stream to `output`, as [`relay`] does with stdio.
async fn relay_between<I, O, S>((mut input, mut out): (I, O), stream: S) -> std::io::Result<()>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite,
{
    let (mut r, mut w) = tokio::io::split(stream);
    let upstream = async {
        tokio::io::copy(&mut input, &mut w).await?;
        w.shutdown().await
    };
    let downstream = async {
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
