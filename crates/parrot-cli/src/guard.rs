//! `parrot guard -- <cmd> [args]`: runs a command that never outlives the guard's parent.

/// Runs the guard with the arguments after `guard` and exits with the child's status.
pub fn run(args: Vec<String>) -> ! {
    let mut args = args.into_iter().peekable();
    if args.peek().map(String::as_str) == Some("--") {
        args.next();
    }
    let Some(program) = args.next() else {
        eprintln!("usage: parrot guard -- <cmd> [args]");
        std::process::exit(2);
    };
    let code = match parrot_guard::run(program.into(), args.map(Into::into).collect()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("parrot guard: {err}");
            127
        }
    };
    std::process::exit(code);
}
