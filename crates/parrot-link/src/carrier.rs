//! Carriers (spec 24.2): how the bytes of a link reach the peer's `parrot connect --link`.
//!
//! A carrier is never trusted: the Noise handshake authenticates the peer whatever carries it.
//! What a carrier does decide is containment (spec 24.4): whether its credentials give a shell on
//! the peer's machine, which would make any link grant meaningless.
//!
//! | Kind | Channel | Containment |
//! | --- | --- | --- |
//! | `ssh` | `ssh <destination> parrot connect --link` | `shell`, unless the key is restricted on the peer |
//! | `command` | Any argv whose stdio reaches the peer's `parrot connect --link` | `shell` |
//! | `tcp` | A TCP connection to the peer's link listener | `restricted` |
//! | Carrier egg | A program that answers `dial` with a channel on its stdio | as declared, else `unknown` |

use crate::LinkError;
use crate::frame::{MIN_MESSAGE, NOISE_MAX};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// How a runtime reaches a peer (`Link.carrier`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarrierRef {
    pub kind: String,
    #[serde(default)]
    pub target: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<CarrierRef>,
}

/// Whether a carrier's credentials reach more of the peer than its link endpoint (spec 24.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Containment {
    /// The carrier can only run `parrot connect --link` on the peer.
    Restricted,
    /// The carrier's credentials give a shell as the runtime's user: full access.
    Shell,
    /// Not declared; treated as `shell`.
    Unknown,
}

impl Containment {
    /// Whether the link grant holds against whoever holds the carrier's credentials.
    pub fn grant_holds(self) -> bool {
        self == Containment::Restricted
    }
}

/// An entry of `carrier/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CarrierInfo {
    pub kind: String,
    pub title: String,
    pub target: Value,
    pub listens: bool,
    pub containment: Containment,
    pub max_message: usize,
}

/// A carrier egg: an external program that opens a channel on its standard input and output.
#[derive(Clone, Debug)]
pub struct EggCarrier {
    pub name: String,
    pub title: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub target_schema: Value,
    pub listens: bool,
    pub max_message: usize,
    pub containment: Option<Containment>,
}

/// An open channel to a peer, not yet authenticated.
pub struct Channel {
    pub stream: Box<dyn Stream>,
    pub kind: String,
    pub containment: Containment,
    /// The largest Noise message the channel carries.
    pub max_message: usize,
    /// The end of the carrier process's standard error, for error details.
    pub stderr: Option<StderrTail>,
}

/// A byte stream a carrier yields.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// The built-in carriers and the registered eggs.
#[derive(Default)]
pub struct Carriers {
    eggs: HashMap<String, EggCarrier>,
}

const BUILTIN: [&str; 3] = ["ssh", "command", "tcp"];

impl Carriers {
    pub fn new() -> Carriers {
        Carriers::default()
    }

    /// Registers an egg. Its name may not shadow a built-in carrier.
    pub fn add_egg(&mut self, egg: EggCarrier) -> Result<(), LinkError> {
        if BUILTIN.contains(&egg.name.as_str()) {
            return Err(LinkError::Carrier(format!("{} is a built-in carrier", egg.name)));
        }
        if egg.max_message < MIN_MESSAGE {
            return Err(LinkError::Carrier(format!(
                "{} has maxMessage under 4 KiB",
                egg.name
            )));
        }
        self.eggs.insert(egg.name.clone(), egg);
        Ok(())
    }

    /// What `carrier/list` returns.
    pub fn list(&self) -> Vec<CarrierInfo> {
        let mut out = builtins();
        let mut eggs: Vec<_> = self.eggs.values().collect();
        eggs.sort_by(|a, b| a.name.cmp(&b.name));
        out.extend(eggs.into_iter().map(|e| CarrierInfo {
            kind: e.name.clone(),
            title: e.title.clone(),
            target: e.target_schema.clone(),
            listens: e.listens,
            containment: e.containment.unwrap_or(Containment::Unknown),
            max_message: e.max_message.min(NOISE_MAX),
        }));
        out
    }

    /// Opens one carrier, without its fallbacks.
    pub async fn open(&self, carrier: &CarrierRef) -> Result<Channel, LinkError> {
        match carrier.kind.as_str() {
            "ssh" => spawn("ssh", ssh_argv(&carrier.target)?, Containment::Shell),
            "command" => spawn("command", command_argv(&carrier.target)?, Containment::Shell),
            "tcp" => tcp(&carrier.target).await,
            kind => match self.eggs.get(kind) {
                Some(egg) => dial_egg(egg, &carrier.target).await,
                None => Err(LinkError::Carrier(format!("unknown carrier {kind}"))),
            },
        }
    }

    /// Opens the carrier, then each fallback in order, and returns the first that opens with its
    /// index (0 for the carrier itself), or the last error.
    pub async fn open_with_fallback(&self, carrier: &CarrierRef) -> Result<(usize, Channel), LinkError> {
        let mut last = None;
        for (i, c) in std::iter::once(carrier).chain(&carrier.fallback).enumerate() {
            match self.open(c).await {
                Ok(channel) => return Ok((i, channel)),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("at least the carrier itself was tried"))
    }
}

/// The built-in carriers as `carrier/list` shows them.
pub fn builtins() -> Vec<CarrierInfo> {
    vec![
        CarrierInfo {
            kind: "ssh".into(),
            title: "SSH".into(),
            target: json!({
                "type": "object",
                "required": ["destination"],
                "properties": {
                    "destination": { "type": "string" },
                    "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
                    "parrot": { "type": "string" },
                },
            }),
            listens: false,
            containment: Containment::Shell,
            max_message: NOISE_MAX,
        },
        CarrierInfo {
            kind: "command".into(),
            title: "Command".into(),
            target: json!({
                "type": "object",
                "required": ["argv"],
                "properties": { "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1 } },
            }),
            listens: false,
            containment: Containment::Shell,
            max_message: NOISE_MAX,
        },
        CarrierInfo {
            kind: "tcp".into(),
            title: "TCP".into(),
            target: json!({
                "type": "object",
                "required": ["address"],
                "properties": { "address": { "type": "string" } },
            }),
            listens: true,
            containment: Containment::Restricted,
            max_message: NOISE_MAX,
        },
    ]
}

fn target_str<'a>(target: &'a Map<String, Value>, field: &str) -> Result<Option<&'a str>, LinkError> {
    match target.get(field) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(LinkError::Carrier(format!("target.{field} must be a string"))),
    }
}

fn no_extra(target: &Map<String, Value>, allowed: &[&str]) -> Result<(), LinkError> {
    match target.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(LinkError::Carrier(format!("unknown target field {k}"))),
        None => Ok(()),
    }
}

/// `ssh -T -o BatchMode=yes [-p port] -- <destination> <parrot> connect --link`. The
/// destination may not start with `-`, so it can't be read as an option, and the remote parrot
/// path is limited to characters a remote shell reads literally.
pub fn ssh_argv(target: &Map<String, Value>) -> Result<Vec<String>, LinkError> {
    no_extra(target, &["destination", "port", "parrot"])?;
    let dest = target_str(target, "destination")?
        .ok_or_else(|| LinkError::Carrier("ssh needs target.destination".into()))?;
    if dest.is_empty() || dest.starts_with('-') || dest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(LinkError::Carrier("bad ssh destination".into()));
    }
    let parrot = target_str(target, "parrot")?.unwrap_or("parrot");
    let literal = |c: char| c.is_ascii_alphanumeric() || "_-./~".contains(c);
    if parrot.is_empty() || parrot.starts_with('-') || !parrot.chars().all(literal) {
        return Err(LinkError::Carrier("bad remote parrot path".into()));
    }
    let mut argv = vec!["ssh".to_owned(), "-T".into(), "-o".into(), "BatchMode=yes".into()];
    match target.get("port") {
        None => {}
        Some(Value::Number(n)) if n.as_u64().is_some_and(|p| (1..=65535).contains(&p)) => {
            argv.extend(["-p".into(), n.to_string()]);
        }
        Some(_) => return Err(LinkError::Carrier("bad ssh port".into())),
    }
    argv.extend([
        "--".into(),
        dest.into(),
        parrot.into(),
        "connect".into(),
        "--link".into(),
    ]);
    Ok(argv)
}

/// The argv of a `command` carrier, run directly without a shell.
pub fn command_argv(target: &Map<String, Value>) -> Result<Vec<String>, LinkError> {
    no_extra(target, &["argv"])?;
    let argv = target
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| LinkError::Carrier("command needs target.argv".into()))?;
    let argv: Option<Vec<String>> = argv.iter().map(|a| a.as_str().map(str::to_owned)).collect();
    match argv {
        Some(argv) if !argv.is_empty() && !argv[0].is_empty() => Ok(argv),
        _ => Err(LinkError::Carrier(
            "target.argv must be a non-empty list of strings".into(),
        )),
    }
}

fn spawn(kind: &str, argv: Vec<String>, containment: Containment) -> Result<Channel, LinkError> {
    let child = ChildStream::spawn(&argv[0], &argv[1..])?;
    let stderr = Some(child.stderr.clone());
    Ok(Channel {
        stream: Box::new(child),
        kind: kind.into(),
        containment,
        max_message: NOISE_MAX,
        stderr,
    })
}

async fn tcp(target: &Map<String, Value>) -> Result<Channel, LinkError> {
    no_extra(target, &["address"])?;
    let addr = target_str(target, "address")?
        .ok_or_else(|| LinkError::Carrier("tcp needs target.address".into()))?;
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| LinkError::Carrier(format!("cannot connect to {addr}: {e}")))?;
    stream.set_nodelay(true)?;
    Ok(Channel {
        stream: Box::new(stream),
        kind: "tcp".into(),
        containment: Containment::Restricted,
        max_message: NOISE_MAX,
        stderr: None,
    })
}

const READY_MAX: usize = 64 * 1024;

/// `<egg> dial`: the target as JSON on the first input line, then `{"ready":true}` or
/// `{"error":…}` on the first output line, then the channel.
async fn dial_egg(egg: &EggCarrier, target: &Map<String, Value>) -> Result<Channel, LinkError> {
    let mut args = egg.args.clone();
    args.push("dial".into());
    let mut child = ChildStream::spawn(egg.program.as_os_str(), &args)?;
    let mut line = serde_json::to_vec(target).expect("JSON values serialize");
    line.push(b'\n');
    child.write_all(&line).await?;
    child.flush().await?;
    // Byte by byte, so nothing of the channel after the line is consumed.
    let mut answer = Vec::new();
    loop {
        let b = child.read_u8().await.map_err(|e| {
            LinkError::Carrier(format!(
                "{} closed before it was ready: {e}; {}",
                egg.name,
                child.stderr.text()
            ))
        })?;
        if b == b'\n' {
            break;
        }
        answer.push(b);
        if answer.len() > READY_MAX {
            return Err(LinkError::Carrier(format!(
                "{}: first line is too long",
                egg.name
            )));
        }
    }
    let answer: Value = serde_json::from_slice(&answer)
        .map_err(|e| LinkError::Carrier(format!("{}: bad first line: {e}", egg.name)))?;
    if answer.get("ready") != Some(&Value::Bool(true)) {
        let detail = answer.get("error").cloned().unwrap_or(answer);
        return Err(LinkError::Carrier(format!("{}: {detail}", egg.name)));
    }
    let stderr = Some(child.stderr.clone());
    Ok(Channel {
        stream: Box::new(child),
        kind: egg.name.clone(),
        containment: egg.containment.unwrap_or(Containment::Unknown),
        max_message: egg.max_message.clamp(MIN_MESSAGE, NOISE_MAX),
        stderr,
    })
}

const STDERR_TAIL: usize = 8 * 1024;

/// The last bytes a carrier process wrote to its standard error.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<Vec<u8>>>);

impl StderrTail {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("stderr lock")).into_owned()
    }

    fn push(&self, bytes: &[u8]) {
        let mut buf = self.0.lock().expect("stderr lock");
        buf.extend_from_slice(bytes);
        let excess = buf.len().saturating_sub(STDERR_TAIL);
        buf.drain(..excess);
    }
}

/// A child process whose standard input and output form the channel. The process is killed when
/// the stream is dropped.
pub struct ChildStream {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: StderrTail,
}

impl ChildStream {
    pub fn spawn<P, A>(program: P, args: &[A]) -> Result<ChildStream, LinkError>
    where
        P: AsRef<std::ffi::OsStr>,
        A: AsRef<std::ffi::OsStr>,
    {
        let name = program.as_ref().to_string_lossy().into_owned();
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| LinkError::Carrier(format!("cannot start {name}: {e}")))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut err = child.stderr.take().expect("stderr is piped");
        let stderr = StderrTail::default();
        let tail = stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                tail.push(&buf[..n]);
            }
        });
        Ok(ChildStream {
            _child: child,
            stdin,
            stdout,
            stderr,
        })
    }
}

impl AsyncRead for ChildStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn ssh_runs_connect_link() {
        let argv = ssh_argv(&target(
            json!({ "destination": "builder@host.example", "port": 2222 }),
        ))
        .unwrap();
        assert_eq!(
            argv,
            [
                "ssh",
                "-T",
                "-o",
                "BatchMode=yes",
                "-p",
                "2222",
                "--",
                "builder@host.example",
                "parrot",
                "connect",
                "--link"
            ]
        );
    }

    #[test]
    fn ssh_refuses_option_injection() {
        for t in [
            json!({ "destination": "-oProxyCommand=touch /tmp/x" }),
            json!({ "destination": "host", "parrot": "parrot; rm -rf ~" }),
            json!({ "destination": "host", "parrot": "$(id)" }),
            json!({ "destination": "host", "port": 0 }),
            json!({ "destination": "host", "extra": true }),
            json!({}),
        ] {
            assert!(ssh_argv(&target(t.clone())).is_err(), "{t}");
        }
    }

    #[test]
    fn command_needs_an_argv() {
        assert!(command_argv(&target(json!({ "argv": [] }))).is_err());
        assert!(command_argv(&target(json!({ "argv": [1] }))).is_err());
        assert_eq!(
            command_argv(&target(
                json!({ "argv": ["docker", "exec", "-i", "box", "parrot"] })
            ))
            .unwrap()[0],
            "docker"
        );
    }

    #[test]
    fn containment_of_the_builtins() {
        let list = Carriers::new().list();
        let of = |k: &str| list.iter().find(|c| c.kind == k).unwrap().containment;
        assert_eq!(of("ssh"), Containment::Shell);
        assert_eq!(of("command"), Containment::Shell);
        assert_eq!(of("tcp"), Containment::Restricted);
        assert!(!Containment::Unknown.grant_holds());
    }

    #[test]
    fn eggs_cannot_shadow_builtins() {
        let egg = EggCarrier {
            name: "ssh".into(),
            title: "fake".into(),
            program: "/bin/false".into(),
            args: vec![],
            target_schema: json!({}),
            listens: false,
            max_message: NOISE_MAX,
            containment: None,
        };
        assert!(Carriers::new().add_egg(egg).is_err());
    }
}
