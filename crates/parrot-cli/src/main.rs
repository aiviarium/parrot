//! The parrot command: serve, connect, driver, guard, conform, mcp.

mod conform;
mod connect;
mod driver;
mod guard;
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
    Conform,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Guard { args } => guard::run(args),
        Command::Conform => conform::run(),
        Command::Serve { state_dir, ws } => block_on(serve::run(state_dir, ws)),
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
