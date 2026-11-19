//! `parrot serve`: the runtime on the local socket, and on WebSocket with `--ws`.

use parrot_core::{Clock, Core, Ids};
use parrot_serve::{Runtime, RuntimeBackend};
use parrot_store::Store;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

/// Overrides the socket path (or pipe name on Windows) for `serve` and `connect`.
pub const SOCKET_ENV: &str = "PARROT_SOCKET";

struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// `<prefix>_` and a UUID v7 in the 26-character ULID alphabet.
struct UlidIds;

impl Ids for UlidIds {
    fn next(&mut self, prefix: &str) -> String {
        const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let v = uuid::Uuid::now_v7().as_u128();
        let mut id = format!("{prefix}_");
        for i in 0..26 {
            id.push(ALPHABET[((v >> (125 - 5 * i)) & 31) as usize] as char);
        }
        id
    }
}

/// The runtime's state directory for this user and OS.
pub fn default_state_dir() -> io::Result<PathBuf> {
    let env_dir = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = || env_dir("HOME").ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"));
    if cfg!(windows) {
        return env_dir("LOCALAPPDATA")
            .map(|d| d.join("parrot"))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"));
    }
    if cfg!(target_os = "macos") {
        return Ok(home()?.join("Library/Application Support/parrot"));
    }
    match env_dir("XDG_STATE_HOME") {
        Some(dir) => Ok(dir.join("parrot")),
        None => Ok(home()?.join(".local/state/parrot")),
    }
}

/// The socket path: `PARROT_SOCKET`, or the platform default.
pub fn socket_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()) {
        return Ok(path.into());
    }
    #[cfg(unix)]
    return parrot_serve::socket::default_socket_path();
    #[cfg(windows)]
    return parrot_serve::socket::default_pipe_name().map(PathBuf::from);
}

pub async fn run(
    state_dir: Option<PathBuf>,
    ws: Option<String>,
    link_listen: Option<String>,
    workspace_root: Option<PathBuf>,
) -> Result<(), String> {
    let dir = match state_dir {
        Some(dir) => dir,
        None => default_state_dir().map_err(|e| e.to_string())?,
    };
    let db = dir.join("state.db");
    let store = Store::open(&db).map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    let key =
        parrot_link::StaticKey::load_or_create(&dir).map_err(|e| format!("cannot read the link key: {e}"))?;
    let link_store = Store::open(&db).map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    let hub = parrot_serve::link_hub::LinkHub::with_store(key, link_store, Some(dir.join("links.json")));
    load_carriers(&dir, &hub);
    let mut core = Core::new(Box::new(SystemClock), Box::new(UlidIds));
    let drivers = load_drivers(&dir);
    parrot_core::agents::load(&mut core.state, drivers.agents());
    core.state.principals.runtime = hub.runtime_id();
    core.state.principals.env = hub.runtime_id().replacen("rt_", "env_", 1);
    // The injected MCP server is this program's `parrot mcp` (spec 26).
    core.state.functions.mcp = std::env::current_exe().ok().map(|p| p.display().to_string());
    if parrot_serve::desktop::available() {
        for spec in parrot_serve::desktop::specs() {
            let name = spec["name"].as_str().unwrap_or_default().to_string();
            core.state.functions.runtime.insert(name, spec);
        }
    }
    let config = parrot_serve::config::Config::load(&dir)?;
    let workspace_root = workspace_root.unwrap_or_else(|| dir.join("workspaces"));
    core.state.principals.roots = roots(&config, &workspace_root);
    for (agent, a) in &config.tui {
        core.state
            .sessions
            .agents
            .insert(agent.clone(), parrot_serve::tui::agent_record(agent, a));
    }
    let scratch = dir.join("files");
    let _ = std::fs::create_dir_all(&scratch);
    let mut targets = parrot_serve::notify::targets_from_env();
    targets.extend(
        config
            .notify
            .iter()
            .filter_map(|t| parrot_serve::notify::parse_target(t)),
    );
    let mut notifier = parrot_serve::notify::Notifier::new(targets);
    notifier.url = config.notify_url.clone();
    if let Some(ca) = &config.notify_ca {
        let pem =
            std::fs::read(dir.join(ca)).map_err(|e| format!("cannot read notifyCa {}: {e}", ca.display()))?;
        notifier
            .trust(&pem)
            .map_err(|e| format!("notifyCa {}: {e}", ca.display()))?;
    }
    let tui = config.tui.clone();
    let tokens = Arc::new(
        parrot_serve::tokens::TokenStore::with_store(
            Store::open(&db).map_err(|e| format!("cannot open {}: {e}", db.display()))?,
            Some(dir.join("tokens.json")),
        )
        .map_err(|e| format!("cannot read the tokens: {e}"))?,
    );
    let backend_tokens = tokens.clone();
    let backend_hub = hub.clone();
    let transfers = dir.join("transfers");
    let (rt, _task) = Runtime::spawn(core, Box::new(SystemClock), move |rt| {
        RuntimeBackend::new(rt, store)
            .with_links(backend_hub)
            .with_transfers(transfers)
            .with_scratch(scratch)
            .with_notifier(notifier)
            .with_drivers(drivers)
            .with_tui(tui)
            .with_tokens(backend_tokens)
            .with_workspace_root(workspace_root)
    });
    hub.attach(rt.clone());

    let socket = socket_path().map_err(|e| e.to_string())?;
    let local = serve_local(rt.clone(), socket.clone());
    let link_tcp = async {
        let Some(addr) = link_listen else {
            return std::future::pending().await;
        };
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
        if let Ok(a) = listener.local_addr() {
            hub.add_address(a.to_string());
        }
        parrot_serve::link_hub::serve_link_tcp(hub.clone(), listener)
            .await
            .map_err(|e| format!("link listener: {e}"))
    };
    let link_local = serve_link_local(hub.clone(), &socket);
    let remote = async {
        let Some(addr) = ws else {
            return std::future::pending().await;
        };
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
        parrot_serve::websocket::serve_websocket(rt.clone(), listener, tokens.clone())
            .await
            .map_err(|e| format!("websocket: {e}"))
    };
    let result = tokio::select! {
        r = local => r,
        r = remote => r,
        r = link_tcp => r,
        r = link_local => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = terminated() => Ok(()),
    };
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket);
    #[cfg(unix)]
    let _ = std::fs::remove_file(parrot_link::endpoint::link_socket_path(&socket));
    result
}

/// The folders client paths must be inside (spec 7, 8): the configuration's `roots`, else the
/// owner's home and the workspace root, which is where workspaces from links land.
fn roots(config: &parrot_serve::config::Config, workspace_root: &std::path::Path) -> Vec<PathBuf> {
    if !config.roots.is_empty() {
        return config.roots.clone();
    }
    let mut roots = Vec::new();
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    if let Some(home) = home {
        roots.push(home);
    }
    if !roots.iter().any(|r| workspace_root.starts_with(r)) {
        roots.push(workspace_root.to_path_buf());
    }
    roots
}

/// The driver eggs of this state directory, with refused ones reported on stderr.
fn load_drivers(dir: &std::path::Path) -> parrot_serve::drivers::Drivers {
    let (drivers, problems) = parrot_serve::drivers::Drivers::load(dir);
    for p in problems {
        eprintln!("parrot: {p}");
    }
    for egg in drivers.eggs() {
        if let Err(e) = &egg.driver {
            eprintln!("parrot: refused a driver egg: {e}");
        }
    }
    drivers
}

/// The link endpoint: `link.sock` beside the socket, or the local pipe with `-link` (E9).
async fn serve_link_local(
    hub: Arc<parrot_serve::link_hub::LinkHub>,
    socket: &std::path::Path,
) -> Result<(), String> {
    let path = parrot_link::endpoint::link_socket_path(socket);
    let listener = parrot_link::endpoint::bind_link_socket(&path)
        .map_err(|e| format!("cannot bind {}: {e}", path.display()))?;
    // Carrier eggs that listen hand their inbound channels to this endpoint.
    match std::env::current_exe() {
        Ok(parrot) => hub.start_listeners(socket.to_path_buf(), parrot),
        Err(e) => eprintln!("parrot: cannot start listening carriers: {e}"),
    }
    parrot_serve::link_hub::serve_link_socket(hub, listener)
        .await
        .map_err(|e| format!("link socket: {e}"))
}

#[cfg(unix)]
async fn serve_local(rt: Runtime, path: PathBuf) -> Result<(), String> {
    let listener = parrot_serve::socket::bind_socket(&path)
        .map_err(|e| format!("cannot bind {}: {e}", path.display()))?;
    parrot_serve::socket::serve_socket(rt, listener)
        .await
        .map_err(|e| format!("socket: {e}"))
}

#[cfg(windows)]
async fn serve_local(rt: Runtime, path: PathBuf) -> Result<(), String> {
    let name = path.to_string_lossy().into_owned();
    parrot_serve::socket::serve_pipe(rt, &name)
        .await
        .map_err(|e| format!("pipe {name}: {e}"))
}

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(windows)]
async fn terminated() {
    std::future::pending().await
}

/// Registers the installed carrier eggs with the link hub; refused ones are reported on stderr.
fn load_carriers(dir: &std::path::Path, hub: &parrot_serve::link_hub::LinkHub) {
    use parrot_link::carrier::{Containment, EggCarrier};
    let (carriers, problems) = parrot_eggs::Eggs::new(dir).carriers();
    for p in problems {
        eprintln!("parrot: {p}");
    }
    for c in carriers {
        let file = match c.carrier {
            Ok(file) => file,
            Err(e) => {
                eprintln!("parrot: refused the carrier egg {}: {e}", c.package);
                continue;
            }
        };
        let egg = EggCarrier {
            name: file.kind.clone(),
            title: file.title,
            program: c.data_dir.join(&file.run),
            args: file.args,
            target_schema: file.target,
            listens: file.listens,
            max_message: file.max_message,
            containment: match file.containment.as_deref() {
                Some("restricted") => Some(Containment::Restricted),
                Some("shell") => Some(Containment::Shell),
                _ => None,
            },
        };
        if let Err(e) = hub.add_carrier(egg) {
            eprintln!("parrot: refused the carrier egg {}: {e}", c.package);
        }
    }
}
