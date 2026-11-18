//! The opencode driver as a standalone binary: serves the driver protocol on stdio (spec 30). Its egg,
//! `eggs/opencode`, ships it as a Hatchery variant per platform.

fn main() {
    parrot_driver::bin::main(
        "parrot-driver-opencode",
        env!("CARGO_PKG_VERSION"),
        parrot_driver_opencode::serve_stdio,
    )
}
