//! The parrot command: serve, connect, driver, guard, conform, mcp.

mod conform;
mod connect;
mod driver;
mod guard;
mod serve;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("guard") => guard::run(args.collect()),
        _ => {
            eprintln!("parrot: not implemented yet");
            std::process::exit(2);
        }
    }
}
