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
    #[cfg(target_os = "linux")] broker_socket: bool,
    #[cfg(target_os = "linux")] driver_state_dir: Option<PathBuf>,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let broker = broker_socket.then(broker_listener).transpose()?;
    #[cfg(not(target_os = "linux"))]
    let broker_socket = false;
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
    if !broker_socket {
        load_carriers(&dir, &hub);
    }
    let mut core = Core::new(Box::new(SystemClock), Box::new(UlidIds));
    #[cfg(not(target_os = "linux"))]
    let driver_state_dir: Option<PathBuf> = None;
    let drivers = load_drivers(driver_state_dir.as_deref().unwrap_or(&dir));
    parrot_core::agents::load(&mut core.state, drivers.agents());
    core.state.principals.runtime = hub.runtime_id();
    core.state.principals.env = hub.runtime_id().replacen("rt_", "env_", 1);
    // The injected MCP server is this program's `parrot mcp` (spec 26).
    if !broker_socket {
        core.state.functions.mcp = std::env::current_exe().ok().map(|p| p.display().to_string());
    }
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

    #[cfg(target_os = "linux")]
    if let Some(listener) = broker {
        return tokio::select! {
            r = parrot_serve::socket::serve_broker(rt, listener) => {
                r.map_err(|e| format!("broker socket: {e}"))
            }
            _ = tokio::signal::ctrl_c() => Ok(()),
            _ = terminated() => Ok(()),
        };
    }

    let socket = socket_path().map_err(|e| e.to_string())?;
    // Bind the local sockets before serving anything, so a second `parrot serve` fails here and
    // never reaches the cleanup below, which would unlink a running runtime's sockets (spec 3.3).
    let bound = bind_local(&socket)?;
    #[cfg(unix)]
    let link_bound = match bind_link(&socket) {
        Ok(listener) => listener,
        Err(e) => {
            // The local socket is this process's own, so take it back before failing.
            let _ = std::fs::remove_file(&socket);
            return Err(e);
        }
    };
    #[cfg(windows)]
    let link_bound = bind_link(&socket)?;
    #[cfg(unix)]
    let local = serve_local(rt.clone(), bound);
    #[cfg(windows)]
    let local = {
        let _ = bound;
        serve_local(rt.clone(), socket.clone())
    };
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
    let link_local = serve_link_local(hub.clone(), &socket, link_bound);
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

/// systemd passes its socket as descriptor 3 and names this process as the recipient.
#[cfg(target_os = "linux")]
fn broker_listener() -> Result<tokio::net::UnixListener, String> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::net::UnixListener;

    let received = std::env::var("LISTEN_PID").ok().and_then(|pid| pid.parse().ok());
    if received != Some(std::process::id()) || std::env::var("LISTEN_FDS").as_deref() != Ok("1") {
        return Err("--broker-socket needs one systemd socket".into());
    }
    // The variables are systemd's alone: a driver this runtime starts must not think that it was
    // handed the same socket.
    // SAFETY: called before the runtime starts its threads, and nothing else reads these.
    for name in ["LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES"] {
        unsafe { std::env::remove_var(name) };
    }

    // SAFETY: fcntl accepts an arbitrary descriptor and returns an error when it is invalid.
    if unsafe { libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(format!(
            "cannot take the systemd socket: {}",
            io::Error::last_os_error()
        ));
    }
    if !is_listening_stream(3)? {
        return Err("systemd socket 3 is not a listening stream socket".into());
    }
    // SAFETY: systemd passed ownership of descriptor 3 and fcntl verified it is open.
    let listener = UnixListener::from(unsafe { OwnedFd::from_raw_fd(3) });
    let address = listener
        .local_addr()
        .map_err(|e| format!("invalid broker socket: {e}"))?;
    if address.as_pathname().is_none() {
        return Err("the broker socket must have a Unix socket path".into());
    }
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    tokio::net::UnixListener::from_std(listener).map_err(|e| e.to_string())
}

/// Whether `fd` is a stream socket that is listening: what systemd passes for a `ListenStream`
/// socket, and nothing else.
#[cfg(target_os = "linux")]
fn is_listening_stream(fd: libc::c_int) -> Result<bool, String> {
    let mut kind: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: a socket's type is an int of the length given, into a place of that size.
    let read = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut len,
        )
    };
    if read < 0 {
        return Err(format!(
            "cannot read systemd socket 3: {}",
            io::Error::last_os_error()
        ));
    }
    if kind != libc::SOCK_STREAM {
        return Ok(false);
    }
    let mut listening: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: the same for whether it takes connections, which is 1 when it does.
    let read = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            (&raw mut listening).cast(),
            &raw mut len,
        )
    };
    if read < 0 {
        return Err(format!(
            "cannot read systemd socket 3: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(listening == 1)
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
#[cfg(unix)]
fn bind_link(socket: &std::path::Path) -> Result<parrot_link::endpoint::LinkListener, String> {
    let path = parrot_link::endpoint::link_socket_path(socket);
    parrot_link::endpoint::bind_link_socket(&path).map_err(|e| format!("cannot bind {}: {e}", path.display()))
}

#[cfg(unix)]
async fn serve_link_local(
    hub: Arc<parrot_serve::link_hub::LinkHub>,
    socket: &std::path::Path,
    listener: parrot_link::endpoint::LinkListener,
) -> Result<(), String> {
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
fn bind_local(path: &std::path::Path) -> Result<tokio::net::UnixListener, String> {
    parrot_serve::socket::bind_socket(path).map_err(|e| format!("cannot bind {}: {e}", path.display()))
}

#[cfg(unix)]
async fn serve_local(rt: Runtime, listener: tokio::net::UnixListener) -> Result<(), String> {
    parrot_serve::socket::serve_socket(rt, listener)
        .await
        .map_err(|e| format!("socket: {e}"))
}

#[cfg(windows)]
fn bind_local(_path: &PathBuf) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn bind_link(_path: &std::path::Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
async fn serve_local(rt: Runtime, path: PathBuf) -> Result<(), String> {
    let name = path.to_string_lossy().into_owned();
    parrot_serve::socket::serve_pipe(rt, &name)
        .await
        .map_err(|e| format!("pipe {name}: {e}"))
}

#[cfg(windows)]
async fn serve_link_local(
    hub: Arc<parrot_serve::link_hub::LinkHub>,
    socket: &std::path::Path,
    _bound: (),
) -> Result<(), String> {
    let path = parrot_link::endpoint::link_socket_path(socket);
    let listener = parrot_link::endpoint::bind_link_socket(&path)
        .map_err(|e| format!("cannot bind {}: {e}", path.display()))?;
    match std::env::current_exe() {
        Ok(parrot) => hub.start_listeners(socket.to_path_buf(), parrot),
        Err(e) => eprintln!("parrot: cannot start listening carriers: {e}"),
    }
    parrot_serve::link_hub::serve_link_socket(hub, listener)
        .await
        .map_err(|e| format!("link socket: {e}"))
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::is_listening_stream;
    use std::os::fd::AsRawFd;

    #[test]
    fn only_a_listening_stream_socket_is_taken_for_systemd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(is_listening_stream(listener.as_raw_fd()).unwrap());

        // A stream socket of a connection takes nothing.
        let client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        assert!(!is_listening_stream(client.as_raw_fd()).unwrap());

        // Nor does a datagram socket, which systemd's `ListenDatagram` would pass.
        // SAFETY: socket returns a new descriptor, closed here, or -1 which the test reports.
        let datagram = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0) };
        assert!(datagram >= 0, "{}", std::io::Error::last_os_error());
        assert!(!is_listening_stream(datagram).unwrap());
        // SAFETY: the descriptor was made above and is not used again.
        unsafe { libc::close(datagram) };

        // A descriptor that is no socket is an error, not a rejection.
        assert!(is_listening_stream(-1).is_err());
    }
}
