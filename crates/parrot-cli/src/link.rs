//! `parrot link`: pairing and link records on the local runtime (spec 24).
//!
//! Each subcommand is one owner call to the local runtime over its socket. A pairing code is a
//! secret, so `add` reads it from standard input, never from argv.

use serde_json::{Map, Value, json};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// The socket path (or pipe name) instead of the default.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: LinkCommand,
}

#[derive(clap::Subcommand)]
enum LinkCommand {
    /// Creates an invite and prints its one-time pairing code.
    Invite {
        /// A name for the link.
        #[arg(long)]
        name: String,
        #[command(flatten)]
        terms: Terms,
        /// When the code expires, as an RFC 3339 time; ten minutes from now by default.
        #[arg(long)]
        expires: Option<String>,
        /// Prints the whole result as JSON, for pairing over a carrier.
        #[arg(long)]
        json: bool,
    },
    /// Dials a peer and pairs with it, with a code from standard input or over the carrier.
    Add {
        /// A name for the link.
        #[arg(long)]
        name: String,
        #[command(flatten)]
        carrier: CarrierArgs,
        #[command(flatten)]
        terms: Terms,
        /// Reads the pairing code from the first line of standard input. Without it, the code
        /// comes from `parrot link invite --json` run on the peer through the carrier.
        #[arg(long)]
        code_stdin: bool,
        /// Scopes to ask the peer for when pairing over the carrier, comma-separated.
        #[arg(long, value_delimiter = ',')]
        ask_grant: Vec<String>,
        /// Which of its sessions to ask the peer to offer: `none`, `linked` or `all`.
        #[arg(long, default_value = "none")]
        ask_sessions: String,
        /// Asks the peer to offer its environment.
        #[arg(long)]
        ask_offer_env: bool,
    },
    /// Lists the links.
    List,
    /// Removes a link and revokes its pairing.
    Remove {
        /// The link id.
        link: String,
    },
}

/// What the peer may do here and what it sees.
#[derive(clap::Args)]
struct Terms {
    /// Scopes the peer gets here, comma-separated, such as `read,prompt`.
    #[arg(long, value_delimiter = ',')]
    grant: Vec<String>,
    /// Offers this runtime's environment to the peer.
    #[arg(long)]
    offer_env: bool,
    /// Which sessions the peer sees: `none`, `linked` or `all`.
    #[arg(long, default_value = "none")]
    sessions: String,
}

#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct CarrierArgs {
    /// Reaches the peer with `ssh <destination> parrot connect --link`.
    #[arg(long)]
    ssh: Option<String>,
    /// Reaches the peer's link listener over TCP, as `host:port`.
    #[arg(long)]
    tcp: Option<String>,
    /// Reaches the peer through this command's standard input and output. It takes every
    /// argument after it up to a `;`, so put it last.
    #[arg(long, num_args = 1.., allow_hyphen_values = true, value_terminator = ";")]
    command: Option<Vec<String>>,
    /// Any carrier as JSON: `{"kind": …, "target": {…}}`.
    #[arg(long)]
    carrier: Option<String>,
}

const SCOPES: [&str; 9] = [
    "read",
    "prompt",
    "answer",
    "configure",
    "manage",
    "delegate",
    "workspace",
    "destructive",
    "functions",
];

pub async fn run(args: Args) -> Result<(), String> {
    let (method, params, print) = request(args.command)?;
    let socket = match args.socket {
        Some(path) => path,
        None => crate::serve::socket_path().map_err(|e| e.to_string())?,
    };
    let result = call(&socket, method, params).await?;
    print(&result);
    Ok(())
}

type Print = fn(&Value);

fn request(command: LinkCommand) -> Result<(&'static str, Value, Print), String> {
    Ok(match command {
        LinkCommand::Invite {
            name,
            terms,
            expires,
            json,
        } => {
            let mut params = terms.params(name)?;
            if let Some(expires) = expires {
                params.insert("expires".into(), expires.into());
            }
            let print: Print = if json { print_json } else { print_invite };
            ("link/invite", params.into(), print)
        }
        LinkCommand::Add {
            name,
            carrier,
            terms,
            code_stdin,
            ask_grant,
            ask_sessions,
            ask_offer_env,
        } => {
            let mut params = terms.params(name.clone())?;
            params.insert("carrier".into(), carrier.into_ref()?);
            if code_stdin {
                params.insert("code".into(), read_code()?.into());
            } else {
                let ask = Terms {
                    grant: ask_grant,
                    offer_env: ask_offer_env,
                    sessions: ask_sessions,
                }
                .params(name)?;
                params.insert("ask".into(), ask.into());
            }
            ("link/add", params.into(), print_pretty)
        }
        LinkCommand::List => ("link/list", json!({}), print_pretty),
        LinkCommand::Remove { link } => ("link/remove", json!({ "link": link }), print_pretty),
    })
}

impl Terms {
    fn params(self, name: String) -> Result<Map<String, Value>, String> {
        if let Some(bad) = self.grant.iter().find(|s| !SCOPES.contains(&s.as_str())) {
            return Err(format!("unknown scope {bad}; scopes are {}", SCOPES.join(", ")));
        }
        if !["none", "linked", "all"].contains(&self.sessions.as_str()) {
            return Err("--sessions is none, linked or all".into());
        }
        let mut params = Map::new();
        params.insert("name".into(), name.into());
        params.insert("grant".into(), self.grant.into());
        params.insert(
            "offer".into(),
            json!({ "env": self.offer_env, "sessions": self.sessions }),
        );
        Ok(params)
    }
}

impl CarrierArgs {
    fn into_ref(self) -> Result<Value, String> {
        let carrier = if let Some(dest) = self.ssh {
            json!({ "kind": "ssh", "target": { "destination": dest } })
        } else if let Some(addr) = self.tcp {
            json!({ "kind": "tcp", "target": { "address": addr } })
        } else if let Some(argv) = self.command {
            json!({ "kind": "command", "target": { "argv": argv } })
        } else if let Some(text) = self.carrier {
            let value: Value = serde_json::from_str(&text).map_err(|e| format!("--carrier: {e}"))?;
            serde_json::from_value::<parrot_link::carrier::CarrierRef>(value.clone())
                .map_err(|e| format!("--carrier: {e}"))?;
            value
        } else {
            return Err("name a carrier".into());
        };
        let parsed: parrot_link::carrier::CarrierRef =
            serde_json::from_value(carrier.clone()).map_err(|e| e.to_string())?;
        // Catches a bad target here rather than in the runtime.
        match parsed.kind.as_str() {
            "ssh" => parrot_link::carrier::ssh_argv(&parsed.target).map(drop),
            "command" => parrot_link::carrier::command_argv(&parsed.target).map(drop),
            _ => Ok(()),
        }
        .map_err(|e| e.to_string())?;
        Ok(carrier)
    }
}

fn read_code() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read the code: {e}"))?;
    let code = line.trim().to_owned();
    parrot_link::pairing::InviteCode::parse(&code).map_err(|e| e.to_string())?;
    Ok(code)
}

fn print_json(v: &Value) {
    println!("{v}");
}

fn print_pretty(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn print_invite(v: &Value) {
    match v.get("code").and_then(Value::as_str) {
        Some(code) => {
            println!("{code}");
            if let Some(id) = v.pointer("/link/id").and_then(Value::as_str) {
                eprintln!("link {id} waits for the peer to pair; the code works once");
            }
        }
        None => print_pretty(v),
    }
}

/// Sends `initialize` and one call as the owner, and returns the call's result.
#[cfg(unix)]
async fn call(socket: &std::path::Path, method: &str, params: Value) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| format!("cannot connect to {}: {e}", socket.display()))?;
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocol": [1],
            "peer": { "kind": "client", "name": "parrot link", "version": env!("CARGO_PKG_VERSION") },
            "features": {},
        },
    });
    let req = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
    for msg in [&init, &req] {
        let mut line = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        line.push(b'\n');
        w.write_all(&line).await.map_err(|e| e.to_string())?;
    }
    w.flush().await.map_err(|e| e.to_string())?;
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the runtime closed the connection")?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = msg.get("id").and_then(Value::as_i64);
        if let Some(err) = msg.get("error").filter(|_| id.is_some()) {
            return Err(describe(method, err));
        }
        if id == Some(2) {
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

#[cfg(windows)]
async fn call(_socket: &std::path::Path, _method: &str, _params: Value) -> Result<Value, String> {
    Err("parrot link is not supported on Windows yet".into())
}

fn describe(method: &str, err: &Value) -> String {
    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = err.get("message").and_then(Value::as_str).unwrap_or("");
    match code {
        -32601 | -32001 => format!("the runtime does not support {method} yet"),
        _ => format!("{method} failed ({code}): {message}"),
    }
}
