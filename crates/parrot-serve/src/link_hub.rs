//! The I/O side of links (spec 24, E9): link records, pinned keys, invites and carriers, the
//! link endpoint, and one runtime connection per connected link.
//!
//! The core asks for link operations with `Effect::Workspace` ops named `link.*` (see
//! `parrot_core::links`); [`LinkHub::op`] runs them. An authenticated link connection becomes a
//! connection of the core with `Peer::Link` and the grant this runtime gave the peer. Each side
//! sends `initialize` as a `runtime` peer first and tells its own core what the peer answered
//! with a `link/peer` notification.

use crate::dispatch::Runtime;
use crate::framing::{Incoming, classify};
use parrot_core::{Input, Peer};
use parrot_link::LinkError;
use parrot_link::carrier::{CarrierRef, Carriers};
use parrot_link::keys::{PinSet, PublicKey, StaticKey};
use parrot_link::noise::{Dialer, LinkConnection, Listener};
use parrot_link::pairing::{DEFAULT_TTL_MS, InviteBook, InviteCode};
use parrot_proto::rpc::RpcError;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

/// The store projection that holds link records.
const LINK_KIND: &str = "link";
/// How long `parrot link invite` may take when it runs through a carrier.
const INVITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The id of the hub's own `initialize` call to the peer.
const INIT_ID: &str = "hub:init";

/// Links of one runtime.
pub struct LinkHub {
    key: Arc<StaticKey>,
    pins: Arc<PinSet>,
    invites: Arc<InviteBook>,
    carriers: Carriers,
    records: Mutex<Vec<Value>>,
    store: Option<Mutex<parrot_store::Store>>,
    addresses: Mutex<Vec<String>>,
    rt: OnceLock<Runtime>,
    live: Mutex<HashMap<String, Arc<Notify>>>,
    /// Handshakes not yet authenticated; more than [`MAX_HANDSHAKES`] at once are refused.
    handshakes: Arc<tokio::sync::Semaphore>,
}

/// How many inbound handshakes may run at once, so unauthenticated peers can't exhaust the runtime.
pub const MAX_HANDSHAKES: usize = 16;
/// How long an inbound handshake may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn link_error(e: LinkError) -> RpcError {
    RpcError::new(-32040, "link failed").with_data(json!({ "reason": e.reason(), "detail": e.to_string() }))
}

fn invalid(message: &str) -> RpcError {
    RpcError::new(-32602, message.to_string())
}

impl LinkHub {
    /// A hub with this runtime's key that keeps its records in memory only.
    pub fn new(key: StaticKey) -> Arc<LinkHub> {
        Self::build(key, Vec::new(), None)
    }

    /// A hub keeping its link records and pinned keys in the runtime's store (spec 35), as
    /// projections of kind `link`. A `links.json` of an older runtime at `legacy` is imported
    /// once, then renamed to `links.json.imported`. Invite secrets are never stored: an invite
    /// lives as long as the runtime that made it.
    pub fn with_store(key: StaticKey, store: parrot_store::Store, legacy: Option<PathBuf>) -> Arc<LinkHub> {
        use parrot_store::Ops;
        let mut records: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store.list(LINK_KIND, cursor.as_deref(), 1000).unwrap_or_default();
            let full = page.len() == 1000;
            cursor = page.last().map(|(id, _)| id.clone());
            records.extend(page.into_iter().map(|(_, v)| v));
            if !full {
                break;
            }
        }
        if let Some(file) = legacy.filter(|f| f.is_file()) {
            let imported: Vec<Value> = std::fs::read(&file)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            for r in imported {
                let Some(id) = r["id"].as_str() else { continue };
                if records.iter().any(|x| x["id"] == id) {
                    continue;
                }
                if store.put(LINK_KIND, id, &r).is_ok() {
                    records.push(r);
                }
            }
            if std::fs::rename(&file, file.with_extension("json.imported")).is_err() {
                eprintln!("parrot: imported {} but cannot rename it", file.display());
            }
        }
        Self::build(key, records, Some(Mutex::new(store)))
    }

    fn build(key: StaticKey, records: Vec<Value>, store: Option<Mutex<parrot_store::Store>>) -> Arc<LinkHub> {
        let pins = Arc::new(PinSet::new());
        for r in &records {
            if let Some(k) = r["peer"]["key"].as_str().and_then(PublicKey::from_hex) {
                pins.pin(k);
            }
        }
        Arc::new(LinkHub {
            key: Arc::new(key),
            pins,
            invites: Arc::new(InviteBook::new()),
            carriers: Carriers::new(),
            records: Mutex::new(records),
            store,
            addresses: Mutex::new(Vec::new()),
            rt: OnceLock::new(),
            live: Mutex::new(HashMap::new()),
            handshakes: Arc::new(tokio::sync::Semaphore::new(MAX_HANDSHAKES)),
        })
    }

    /// This runtime's id, derived from its key.
    pub fn runtime_id(&self) -> String {
        self.key.runtime_id()
    }

    /// Addresses put into invite codes, such as the TCP listener's.
    pub fn add_address(&self, address: String) {
        self.addresses.lock().expect("lock").push(address);
    }

    /// Connects the hub to its runtime and dials every link that has a carrier.
    pub fn attach(self: &Arc<Self>, rt: Runtime) {
        if self.rt.set(rt).is_err() {
            return;
        }
        let dial: Vec<String> = self
            .records
            .lock()
            .expect("lock")
            .iter()
            .filter(|r| r.get("carrier").is_some_and(Value::is_object) && r["peer"]["key"].is_string())
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();
        for link in dial {
            tokio::spawn(self.clone().keep_dialled::<tokio::io::DuplexStream>(link, None));
        }
    }

    fn save(&self, records: &[Value]) {
        use parrot_store::Ops;
        let Some(store) = &self.store else { return };
        let mut store = store.lock().expect("lock");
        let saved = store.transaction(|tx| -> parrot_store::Result<()> {
            let stored: Vec<String> = tx
                .list(LINK_KIND, None, 100_000)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            for id in stored {
                if !records.iter().any(|r| r["id"] == id.as_str()) {
                    tx.delete(LINK_KIND, &id)?;
                }
            }
            for r in records {
                if let Some(id) = r["id"].as_str() {
                    tx.put(LINK_KIND, id, r)?;
                }
            }
            Ok(())
        });
        if let Err(e) = saved {
            eprintln!("parrot: cannot store the links: {e}");
        }
    }

    fn update<T>(&self, f: impl FnOnce(&mut Vec<Value>) -> T) -> T {
        let mut records = self.records.lock().expect("lock");
        let out = f(&mut records);
        self.save(&records);
        out
    }

    fn record(&self, link: &str) -> Option<Value> {
        self.records
            .lock()
            .expect("lock")
            .iter()
            .find(|r| r["id"] == link)
            .cloned()
    }

    fn set_status(&self, link: &str, status: &str, via: Option<&str>) {
        self.update(|rs| {
            if let Some(r) = rs.iter_mut().find(|r| r["id"] == link) {
                r["status"] = json!(status);
                if let Some(via) = via {
                    r["via"] = json!(via);
                }
                r["since"] = json!(parrot_core::sessions::timestamp(now_ms()));
            }
        });
    }

    /// The `Link` JSON of spec 24.1, without what the hub keeps for itself.
    fn public(record: &Value) -> Value {
        let mut r = record.clone();
        if let Some(o) = r.as_object_mut() {
            o.remove("invite");
        }
        r
    }

    /// Runs one `link.*` operation for the core.
    pub async fn op(self: Arc<Self>, op: &str, params: Value) -> Result<Value, RpcError> {
        match op {
            "link.list" => Ok(json!({
                "links": self.records.lock().expect("lock").iter().map(Self::public).collect::<Vec<_>>()
            })),
            "carrier.list" => Ok(json!({ "carriers": self.carriers.list() })),
            "link.invite" => self.invite(params),
            "link.add" => self.add(params).await,
            "link.update" => self.update_link(params),
            "link.remove" => self.remove(params),
            _ => Err(RpcError::new(-32601, format!("unknown link operation {op}"))),
        }
    }

    fn invite(&self, p: Value) -> Result<Value, RpcError> {
        let name = p["name"].as_str().ok_or_else(|| invalid("name is required"))?;
        let ttl = p["expires"]
            .as_str()
            .and_then(parrot_core::requests::parse_rfc3339_ms)
            .map(|at| at.saturating_sub(now_ms()))
            .unwrap_or(DEFAULT_TTL_MS);
        let addresses = self.addresses.lock().expect("lock").clone();
        let code = self.invites.create(self.key.public(), addresses, ttl, now_ms());
        let link = json!({
            "id": new_link_id(),
            "name": name,
            "peer": { "runtime": "", "key": "" },
            "grant": p.get("grant").cloned().unwrap_or_else(|| json!([])),
            "offer": p.get("offer").cloned().unwrap_or_else(|| json!({ "env": false, "sessions": "none" })),
            "containment": "restricted",
            "status": "unpaired",
            "invite": code.invite,
        });
        self.update(|rs| rs.push(link.clone()));
        Ok(json!({ "code": code.encode(), "link": Self::public(&link) }))
    }

    async fn add(self: Arc<Self>, p: Value) -> Result<Value, RpcError> {
        let name = p["name"].as_str().ok_or_else(|| invalid("name is required"))?;
        let carrier: CarrierRef =
            serde_json::from_value(p["carrier"].clone()).map_err(|e| invalid(&format!("carrier: {e}")))?;
        let code = match p["code"].as_str() {
            Some(code) => InviteCode::parse(code).map_err(link_error)?,
            None => invite_over_carrier(&carrier, name, &p).await?,
        };
        let channel = self.carriers.open(&carrier).await.map_err(link_error)?;
        let conn = Dialer::new(self.key.clone())
            .max_message(channel.max_message)
            .pair(channel.stream, &code)
            .await
            .map_err(link_error)?;
        self.pins.pin(conn.peer.key);
        let id = new_link_id();
        let link = json!({
            "id": id,
            "name": name,
            "peer": { "runtime": conn.peer.runtime, "key": conn.peer.key.to_hex() },
            "carrier": carrier,
            "grant": p.get("grant").cloned().unwrap_or_else(|| json!([])),
            "offer": p.get("offer").cloned().unwrap_or_else(|| json!({ "env": false, "sessions": "none" })),
            "containment": channel.containment,
            "status": "connected",
            "via": channel.kind,
            "since": parrot_core::sessions::timestamp(now_ms()),
        });
        self.update(|rs| rs.push(link.clone()));
        tokio::spawn(self.clone().keep_dialled(id, Some(conn)));
        Ok(json!({ "link": Self::public(&link) }))
    }

    fn update_link(&self, p: Value) -> Result<Value, RpcError> {
        let link = p["link"].as_str().ok_or_else(|| invalid("link is required"))?;
        self.update(|rs| {
            let r = rs
                .iter_mut()
                .find(|r| r["id"] == link)
                .ok_or_else(|| RpcError::new(-32002, "link not found"))?;
            for field in ["carrier", "grant", "offer"] {
                if let Some(v) = p.get(field) {
                    r[field] = v.clone();
                }
            }
            Ok(json!({ "link": Self::public(r) }))
        })
    }

    fn remove(&self, p: Value) -> Result<Value, RpcError> {
        let link = p["link"].as_str().ok_or_else(|| invalid("link is required"))?;
        let removed = self.update(|rs| {
            let i = rs.iter().position(|r| r["id"] == link)?;
            Some(rs.remove(i))
        });
        let removed = removed.ok_or_else(|| RpcError::new(-32002, "link not found"))?;
        if let Some(runtime) = removed["peer"]["runtime"].as_str() {
            self.pins.unpin(runtime);
        }
        if let Some(invite) = removed["invite"].as_str() {
            self.invites.revoke(invite);
        }
        if let Some(kill) = self.live.lock().expect("lock").remove(link) {
            kill.notify_waiters();
        }
        Ok(json!({}))
    }

    /// Serves a dialled link, then dials it again with backoff while the record has a carrier.
    async fn keep_dialled<S>(self: Arc<Self>, link: String, first: Option<LinkConnection<S>>)
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        if let Some(conn) = first {
            self.clone().serve_conn(link.clone(), conn, None).await;
        }
        let mut backoff = Duration::from_secs(1);
        loop {
            let Some(record) = self.record(&link) else { return };
            let (Ok(carrier), Some(key)) = (
                serde_json::from_value::<CarrierRef>(record["carrier"].clone()),
                record["peer"]["key"].as_str().and_then(PublicKey::from_hex),
            ) else {
                return;
            };
            self.set_status(&link, "connecting", None);
            if let Ok((_, channel)) = self.carriers.open_with_fallback(&carrier).await {
                let via = channel.kind.clone();
                match Dialer::new(self.key.clone())
                    .max_message(channel.max_message)
                    .dial(channel.stream, &key)
                    .await
                {
                    Ok(conn) => {
                        backoff = Duration::from_secs(1);
                        self.clone().serve_conn(link.clone(), conn, Some(via)).await;
                        continue;
                    }
                    // A key that doesn't match waits for the owner (spec 24.3).
                    Err(LinkError::Key { .. }) => {
                        self.set_status(&link, "down", None);
                        return;
                    }
                    Err(_) => {}
                }
            }
            self.set_status(&link, "down", None);
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(300));
        }
    }

    /// Authenticates one inbound stream from the link endpoint and serves it.
    pub async fn accept<S>(self: Arc<Self>, stream: S)
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        // Over the cap the stream is dropped before any work is done for it.
        let Ok(permit) = self.handshakes.clone().try_acquire_owned() else {
            return;
        };
        let listener = Listener::new(self.key.clone(), self.pins.clone(), self.invites.clone());
        let accepted = tokio::time::timeout(HANDSHAKE_TIMEOUT, listener.accept(stream)).await;
        drop(permit);
        let Ok(Ok(conn)) = accepted else {
            return;
        };
        let link = match &conn.paired {
            Some(invite) => {
                self.pins.pin(conn.peer.key);
                let peer = json!({ "runtime": conn.peer.runtime, "key": conn.peer.key.to_hex() });
                self.update(|rs| {
                    let r = rs.iter_mut().find(|r| r["invite"] == invite.as_str())?;
                    r["peer"] = peer;
                    if let Some(o) = r.as_object_mut() {
                        o.remove("invite");
                    }
                    r["id"].as_str().map(str::to_string)
                })
            }
            None => self
                .records
                .lock()
                .expect("lock")
                .iter()
                .find(|r| r["peer"]["runtime"] == conn.peer.runtime.as_str())
                .and_then(|r| r["id"].as_str().map(str::to_string)),
        };
        if let Some(link) = link {
            self.serve_conn(link, conn, Some("endpoint".into())).await;
        }
    }

    /// Serves one authenticated connection as a `Peer::Link` connection of the core.
    async fn serve_conn<S>(self: Arc<Self>, link: String, conn: LinkConnection<S>, via: Option<String>)
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let Some(rt) = self.rt.get().cloned() else { return };
        let Some(record) = self.record(&link) else { return };
        let scopes: Vec<String> = record["grant"]
            .as_array()
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let Ok(mut c) = rt
            .connect(Peer::Link {
                link: link.clone(),
                scopes,
            })
            .await
        else {
            return;
        };
        let stop = Arc::new(Notify::new());
        if let Some(old) = self.live.lock().expect("lock").insert(link.clone(), stop.clone()) {
            old.notify_waiters();
        }
        self.set_status(&link, "connected", via.as_deref());
        let LinkConnection {
            mut reader,
            mut writer,
            ..
        } = conn;
        let init = json!({ "jsonrpc": "2.0", "id": INIT_ID, "method": "initialize", "params": {
            "protocol": [1],
            "peer": { "kind": "runtime", "name": "parrot", "version": env!("CARGO_PKG_VERSION") },
            "features": {},
        }});
        let id = c.id.clone();
        let read = async {
            loop {
                let Ok(Some(bytes)) = reader.recv().await else {
                    return;
                };
                let Ok(text) = String::from_utf8(bytes) else {
                    return;
                };
                if let Ok(v) = serde_json::from_str::<Value>(&text)
                    && v["id"] == INIT_ID
                    && v.get("method").is_none()
                {
                    let r = &v["result"]["runtime"];
                    let input = Input::Call {
                        conn: id.clone(),
                        id: None,
                        method: "link/peer".into(),
                        params: json!({ "runtime": r["id"], "env": r["env"] }),
                    };
                    if !rt.incoming(&id, Incoming::Input(input)).await {
                        return;
                    }
                    continue;
                }
                if !rt.incoming(&id, classify(&id, &text)).await {
                    return;
                }
            }
        };
        let rx = &mut c.rx;
        let write = async {
            if writer.send(init.to_string().as_bytes()).await.is_err() {
                return;
            }
            while let Some(line) = rx.recv().await {
                if writer.send(line.as_bytes()).await.is_err() {
                    return;
                }
            }
        };
        let kill = c.kill.clone();
        tokio::select! {
            _ = read => {}
            _ = write => {}
            _ = kill.notified() => {}
            _ = stop.notified() => {}
        }
        rt.disconnect(&id).await;
        let _ = writer.shutdown().await;
        let mut live = self.live.lock().expect("lock");
        if live.get(&link).is_some_and(|s| Arc::ptr_eq(s, &stop)) {
            live.remove(&link);
            drop(live);
            self.set_status(&link, "down", None);
        }
    }
}

fn new_link_id() -> String {
    crate::dispatch::new_conn_id().replacen("con_", "lnk_", 1)
}

/// Accepts link candidates on the link endpoint `link.sock` forever.
#[cfg(unix)]
pub async fn serve_link_socket(hub: Arc<LinkHub>, listener: tokio::net::UnixListener) -> std::io::Result<()> {
    loop {
        let stream = parrot_link::endpoint::accept_same_user(&listener).await?;
        tokio::spawn(hub.clone().accept(stream));
    }
}

/// Accepts link candidates over TCP (`parrot serve --link-listen`); the handshake authenticates.
pub async fn serve_link_tcp(hub: Arc<LinkHub>, listener: tokio::net::TcpListener) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(hub.clone().accept(stream));
    }
}

/// A value that may stand in a remote command line as it is: a shell on the peer reads it as one
/// literal word.
fn literal(v: &str) -> bool {
    !v.is_empty() && !v.starts_with('-') && v.chars().all(|c| c.is_ascii_alphanumeric() || "_-.,".contains(c))
}

/// The argv that runs `parrot link invite --json` on the peer through `carrier` (spec 24.3), for
/// carriers that reach a shell there: `ssh`, and a `command` whose argv ends in
/// `parrot connect --link`.
pub fn invite_argv(carrier: &CarrierRef, name: &str, ask: &Value) -> Result<Vec<String>, RpcError> {
    let refuse = |why: &str| {
        RpcError::new(-32001, format!("pairing over this carrier needs a code: {why}"))
            .with_data(json!({ "feature": "link.pairCarrier" }))
    };
    let mut argv = match carrier.kind.as_str() {
        "ssh" => parrot_link::carrier::ssh_argv(&carrier.target).map_err(link_error)?,
        "command" => parrot_link::carrier::command_argv(&carrier.target).map_err(link_error)?,
        _ => return Err(refuse("the carrier gives no shell on the peer")),
    };
    let at = argv
        .windows(2)
        .position(|w| w[0] == "connect" && w[1] == "--link")
        .ok_or_else(|| refuse("the command does not run parrot connect --link"))?;
    let rest: Vec<String> = argv.split_off(at).into_iter().skip(2).collect();
    let grant: Vec<&str> = ask["grant"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let sessions = ask["offer"]["sessions"].as_str().unwrap_or("none");
    if !literal(name) || grant.iter().any(|g| !literal(g)) || !["none", "linked", "all"].contains(&sessions) {
        return Err(invalid("name and ask must be plain words"));
    }
    argv.extend([
        "link".into(),
        "invite".into(),
        "--json".into(),
        "--name".into(),
        name.into(),
    ]);
    if !grant.is_empty() {
        argv.extend(["--grant".into(), grant.join(",")]);
    }
    argv.extend(["--sessions".into(), sessions.into()]);
    if ask["offer"]["env"].as_bool() == Some(true) {
        argv.push("--offer-env".into());
    }
    // The socket of the peer's `connect --link`, when it names one, is the runtime to ask.
    let mut rest = rest.into_iter();
    while let Some(arg) = rest.next() {
        if arg == "--socket"
            && let Some(path) = rest.next()
        {
            argv.extend([arg, path]);
        }
    }
    Ok(argv)
}

/// Runs `parrot link invite --json` through the carrier and reads the code it prints. The
/// carrier is trusted exactly this once; every connection after it checks the pinned key.
async fn invite_over_carrier(carrier: &CarrierRef, name: &str, p: &Value) -> Result<InviteCode, RpcError> {
    let peer_name = p["ask"]["name"].as_str().unwrap_or(name);
    let argv = invite_argv(carrier, peer_name, &p["ask"])?;
    let failed = |why: String| {
        RpcError::new(-32040, "link failed").with_data(json!({ "reason": "pairing", "detail": why }))
    };
    let run = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(INVITE_TIMEOUT, run)
        .await
        .map_err(|_| failed("the invite timed out".into()))?
        .map_err(|e| failed(format!("cannot run the carrier: {e}")))?;
    if !out.status.success() {
        return Err(failed(format!("the invite failed: {}", out.status)));
    }
    // The output holds the code, a secret, so it never goes into an error.
    let v: Value =
        serde_json::from_slice(&out.stdout).map_err(|_| failed("the invite printed no JSON".into()))?;
    let code = v["code"]
        .as_str()
        .ok_or_else(|| failed("the invite printed no code".into()))?;
    InviteCode::parse(code).map_err(link_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(argv: &[&str]) -> CarrierRef {
        serde_json::from_value(json!({ "kind": "command", "target": { "argv": argv } })).unwrap()
    }

    #[test]
    fn invites_through_a_command_that_reaches_parrot() {
        let c = command(&[
            "docker",
            "exec",
            "-i",
            "box",
            "parrot",
            "connect",
            "--link",
            "--socket",
            "/run/p/sock",
        ]);
        let ask = json!({ "grant": ["read", "prompt"], "offer": { "env": true, "sessions": "linked" } });
        let argv = invite_argv(&c, "laptop", &ask).unwrap();
        assert_eq!(
            argv,
            [
                "docker",
                "exec",
                "-i",
                "box",
                "parrot",
                "link",
                "invite",
                "--json",
                "--name",
                "laptop",
                "--grant",
                "read,prompt",
                "--sessions",
                "linked",
                "--offer-env",
                "--socket",
                "/run/p/sock"
            ]
        );
    }

    #[test]
    fn refuses_what_a_remote_shell_would_read_as_code() {
        let c = command(&["parrot", "connect", "--link"]);
        for ask in [
            json!({ "grant": ["read;rm"] }),
            json!({ "grant": ["read"], "offer": { "sessions": "$(x)" } }),
        ] {
            assert_eq!(invite_argv(&c, "laptop", &ask).unwrap_err().code, -32602, "{ask}");
        }
        assert_eq!(invite_argv(&c, "a b", &json!({})).unwrap_err().code, -32602);
        let tcp: CarrierRef =
            serde_json::from_value(json!({ "kind": "tcp", "target": { "address": "x:1" } })).unwrap();
        assert_eq!(invite_argv(&tcp, "laptop", &json!({})).unwrap_err().code, -32001);
        let other = command(&["socat", "-", "TCP:x:1"]);
        assert_eq!(
            invite_argv(&other, "laptop", &json!({})).unwrap_err().code,
            -32001
        );
    }

    #[test]
    fn imports_links_json_once_into_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("links.json");
        let record = json!({ "id": "lnk_a", "name": "made-up", "peer": { "runtime": "rt_x", "key": "00".repeat(32) },
            "grant": ["read"], "offer": { "env": false, "sessions": "none" }, "containment": "restricted", "status": "down" });
        std::fs::write(&legacy, serde_json::to_vec(&json!([record])).unwrap()).unwrap();
        let db = dir.path().join("state.db");
        let key = || StaticKey::from_private([7; 32]);
        let hub = LinkHub::with_store(
            key(),
            parrot_store::Store::open(&db).unwrap(),
            Some(legacy.clone()),
        );
        assert!(hub.record("lnk_a").is_some());
        assert!(!legacy.exists());
        assert!(dir.path().join("links.json.imported").exists());
        let pinned = PublicKey::from_hex(&"00".repeat(32)).unwrap().runtime_id();
        assert!(hub.pins.get(&pinned).is_some());
        hub.remove(json!({ "link": "lnk_a" })).unwrap();
        drop(hub);
        let again = LinkHub::with_store(key(), parrot_store::Store::open(&db).unwrap(), Some(legacy));
        assert!(again.record("lnk_a").is_none());
    }
}
