//! `guard -- <cmd> [args]`: the guard as a standalone binary, for tests.

fn main() {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--") {
        args.remove(0);
    }
    if args.is_empty() {
        eprintln!("usage: guard -- <cmd> [args]");
        std::process::exit(2);
    }
    let program = args.remove(0);
    match parrot_guard::run(program, args) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("guard: {err}");
            std::process::exit(127);
        }
    }
}
