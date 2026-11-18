//! The claude driver as a standalone binary: serves the driver protocol on stdio (spec 30). Its egg,
//! `eggs/claude`, ships it as a Hatchery variant per platform.

fn main() {
    parrot_driver::bin::main(
        "parrot-driver-claude",
        env!("CARGO_PKG_VERSION"),
        parrot_driver_claude::serve_stdio,
    )
}
