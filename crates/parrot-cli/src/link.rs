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
        /// The peer's SSH public key, written to `authorized_keys` so that it can only run
        /// `parrot connect --link` here. The peer then pairs with the code over `ssh`.
        #[arg(long, value_name = "KEY")]
        authorized_key: Option<String>,
        /// The `authorized_keys` file to write instead of `~/.ssh/authorized_keys`.
        #[arg(long, value_name = "PATH", requires = "authorized_key")]
        authorized_keys: Option<PathBuf>,
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
    // The key line is written once the runtime made the invite, so a refused invite leaves the
    // file as it was.
    let restrict = match &args.command {
        LinkCommand::Invite {
            authorized_key: Some(key),
            authorized_keys,
            ..
        } => {
            let line = restricted_line(key, &parrot_command())?;
            let file = match authorized_keys {
                Some(f) => f.clone(),
                None => default_authorized_keys()?,
            };
            Some((line, key.clone(), file))
        }
        _ => None,
    };
    let (method, params, print) = request(args.command)?;
    let socket = match args.socket {
        Some(path) => path,
        None => crate::serve::socket_path().map_err(|e| e.to_string())?,
    };
    let result = call(&socket, method, params).await?;
    if let Some((line, key, file)) = restrict {
        write_authorized_key(&file, &key, &line)?;
        eprintln!(
            "the key may only run parrot connect --link here, written to {}",
            file.display()
        );
    }
    print(&result);
    Ok(())
}

/// What the restricted key runs: this binary when its path is plain, else `parrot` on the PATH.
fn parrot_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .filter(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| !c.is_whitespace() && !matches!(c, '"' | '\\' | '\''))
        })
        .unwrap_or_else(|| "parrot".into())
}

/// The `authorized_keys` line that lets `key` run only `parrot connect --link` (spec 24.4).
pub fn restricted_line(key: &str, parrot: &str) -> Result<String, String> {
    let bad =
        || "--authorized-key takes an OpenSSH public key: a key type, its base64 and a comment".to_string();
    if key.contains(['\n', '\r', '"']) {
        return Err(bad());
    }
    let mut fields = key.split_whitespace();
    let (Some(kind), Some(blob)) = (fields.next(), fields.next()) else {
        return Err(bad());
    };
    let known = kind.starts_with("ssh-") || kind.starts_with("ecdsa-") || kind.starts_with("sk-");
    let base64 = blob
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='));
    if !known || !base64 {
        return Err(bad());
    }
    let comment: Vec<&str> = fields.collect();
    let mut line = format!("command=\"{parrot} connect --link\",restrict {kind} {blob}");
    if !comment.is_empty() {
        line.push(' ');
        line.push_str(&comment.join(" "));
    }
    Ok(line)
}

fn default_authorized_keys() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or("HOME is not set; name the file with --authorized-keys")?;
    Ok(PathBuf::from(home).join(".ssh").join("authorized_keys"))
}

/// Writes `line` for `key`, replacing every other line for the same key: sshd takes the first
/// line that matches, so an older unrestricted line would still give a shell.
pub fn write_authorized_key(file: &std::path::Path, key: &str, line: &str) -> Result<(), String> {
    let blob = key.split_whitespace().nth(1).unwrap_or_default();
    let old = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let mut lines: Vec<&str> = old
        .lines()
        .filter(|l| !l.split_whitespace().any(|f| f == blob))
        .collect();
    lines.push(line);
    let text = lines.join("\n") + "\n";
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = file.with_extension("parrot-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, file).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

type Print = fn(&Value);

fn request(command: LinkCommand) -> Result<(&'static str, Value, Print), String> {
    Ok(match command {
        LinkCommand::Invite {
            name,
            terms,
            expires,
            json,
            authorized_key,
            ..
        } => {
            let mut params = terms.params(name)?;
            if let Some(expires) = expires {
                params.insert("expires".into(), expires.into());
            }
            if authorized_key.is_some() {
                params.insert("authorizedKey".into(), true.into());
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMadeUpMadeUpMadeUpMadeUpMadeUpMadeUp peer@example.invalid";

    #[test]
    fn the_line_restricts_the_key_to_connect_link() {
        let line = restricted_line(KEY, "/opt/parrot/bin/parrot").unwrap();
        assert_eq!(
            line,
            "command=\"/opt/parrot/bin/parrot connect --link\",restrict ssh-ed25519 \
             AAAAC3NzaC1lZDI1NTE5AAAAIMadeUpMadeUpMadeUpMadeUpMadeUpMadeUp peer@example.invalid"
        );
        for bad in [
            "",
            "ssh-ed25519",
            "rsa AAAA",
            "ssh-ed25519 AA\"AA",
            "ssh-ed25519 AAAA\nssh-rsa BBBB",
            "ssh-ed25519 AA;rm",
        ] {
            assert!(restricted_line(bad, "parrot").is_err(), "{bad}");
        }
    }

    #[test]
    fn writing_replaces_an_unrestricted_line_for_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".ssh/authorized_keys");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let other =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherOtherOtherOtherOtherOtherOther other@example.invalid";
        std::fs::write(&file, format!("{other}\n{KEY}\n")).unwrap();
        let line = restricted_line(KEY, "parrot").unwrap();
        write_authorized_key(&file, KEY, &line).unwrap();
        write_authorized_key(&file, KEY, &line).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, format!("{other}\n{line}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
