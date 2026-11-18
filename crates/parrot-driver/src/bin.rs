//! The `main` of a standalone driver binary, such as `parrot-driver-claude`: it serves the driver
//! protocol on stdio (spec 30), and answers `guard -- <cmd>` so it can start its harness under the
//! guard without needing `parrot` beside it (E4).

use std::future::Future;

/// Runs a driver binary: `<exe> guard -- <cmd> [args]` runs the guard, `<exe> --version` prints
/// the version, and anything else serves `serve` on stdio. Exits the process.
pub fn main<F, Fut>(name: &str, version: &str, serve: F) -> !
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<()>>,
{
    let mut args = std::env::args_os().skip(1).peekable();
    match args.peek().and_then(|a| a.to_str()) {
        Some("guard") => {
            args.next();
            if args.peek().and_then(|a| a.to_str()) == Some("--") {
                args.next();
            }
            let Some(program) = args.next() else {
                eprintln!("usage: {name} guard -- <cmd> [args]");
                std::process::exit(2);
            };
            let code = parrot_guard::run(program, args.collect()).unwrap_or_else(|err| {
                eprintln!("{name} guard: {err}");
                127
            });
            std::process::exit(code);
        }
        Some("--version") => {
            println!("{name} {version}");
            std::process::exit(0);
        }
        _ => {}
    }
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("{name}: cannot start tokio: {err}");
            std::process::exit(1);
        }
    };
    match rt.block_on(serve()) {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            eprintln!("{name}: {err}");
            std::process::exit(1);
        }
    }
}

/// A boxed reply to one driver call.
pub type Reply = std::pin::Pin<Box<dyn Future<Output = Result<serde_json::Value, parrot_proto::rpc::RpcError>> + Send>>;

/// Serves JSON-RPC lines on stdin and stdout: each request goes to `handle` on its own task, and
/// each value from `updates` is sent as an `update` notification. Returns when stdin closes.
pub async fn serve_stdio<H>(
    handle: H,
    mut updates: tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
) -> std::io::Result<()>
where
    H: Fn(String, serde_json::Value) -> Reply + Send + Sync + 'static,
{
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = out_rx.recv().await {
            let mut line = msg.to_string();
            line.push('\n');
            if stdout.write_all(line.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });
    let notes = out_tx.clone();
    tokio::spawn(async move {
        while let Some(u) = updates.recv().await {
            if notes
                .send(json!({"jsonrpc": "2.0", "method": "update", "params": u}))
                .is_err()
            {
                break;
            }
        }
    });
    let handle = std::sync::Arc::new(handle);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(method), Some(id)) = (msg["method"].as_str(), msg.get("id").cloned()) else {
            continue;
        };
        let reply = handle(method.to_owned(), msg.get("params").cloned().unwrap_or(json!({})));
        let out = out_tx.clone();
        tokio::spawn(async move {
            let msg = match reply.await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
            };
            let _ = out.send(msg);
        });
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}
