//! The parrot command: serve, connect, driver, guard, conform, mcp.

mod acp;
mod client;
mod conform;
mod connect;
mod driver;
mod eggs;
mod guard;
mod link;
mod mcp;
mod probe;
mod serve;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "parrot",
    version,
    about = "Runs agent CLIs and serves the Parrot protocol"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Runs the runtime on the local socket, and on WebSocket when `--ws` is given.
    Serve {
        /// The state directory; the database is `state.db` in it.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// A loopback address to serve WebSocket on, such as `127.0.0.1:7777`.
        #[arg(long)]
        ws: Option<String>,
        /// An address to accept links on over TCP, such as `0.0.0.0:7421`.
        #[arg(long)]
        link_listen: Option<String>,
        /// Where workspaces that arrive over a link go; `workspaces` in the state directory
        /// by default.
        #[arg(long)]
        workspace_root: Option<PathBuf>,
    },
    /// Relays standard input and output to the local runtime's socket.
    Connect {
        /// The socket path (or pipe name) instead of the default.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Connect as a link carrier.
        #[arg(long)]
        link: bool,
    },
    /// Serves a built-in driver on standard input and output.
    Driver {
        /// The driver: `claude`.
        name: String,
    },
    /// Runs a command that never outlives the guard's parent.
    Guard {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Runs the conformance suite.
    Conform(conform::Args),
    /// Serves the MCP server for a binding on standard input and output.
    Mcp(mcp::Args),
    /// Pairs with other runtimes and manages links.
    Link(link::Args),
    /// Installs, lists and checks driver eggs.
    Eggs(eggs::Args),
    /// Runs a probe scenario against a real CLI and records what it emits.
    Probe(probe::Args),
    /// Serves a runtime session as an ACP agent on standard input and output.
    Acp(acp::Args),
    #[command(flatten)]
    Client(client::Command),
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Guard { args } => guard::run(args),
        Command::Conform(args) => conform::run(args),
        Command::Mcp(args) => block_on(mcp::run(args)),
        Command::Link(args) => block_on(link::run(args)),
        Command::Client(command) => block_on(client::run(command)),
        Command::Eggs(args) => block_on(eggs::run(args)),
        Command::Probe(args) => block_on(probe::run(args)),
        Command::Acp(args) => block_on(acp::run(args)),
        Command::Serve {
            state_dir,
            ws,
            link_listen,
            workspace_root,
        } => block_on(serve::run(state_dir, ws, link_listen, workspace_root)),
        Command::Connect { socket, link } => block_on(connect::run(socket, link)),
        Command::Driver { name } => block_on(driver::run(name)),
    };
    std::process::exit(code);
}

fn block_on(fut: impl Future<Output = Result<(), String>>) -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("parrot: cannot start tokio: {err}");
            return 1;
        }
    };
    match rt.block_on(fut) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("parrot: {err}");
            1
        }
    }
}
