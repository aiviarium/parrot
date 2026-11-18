//! The t3 driver as a standalone binary: serves the driver protocol on stdio (spec 30). Its egg,
//! `eggs/t3`, ships it as a Hatchery variant per platform.

fn main() {
    parrot_driver::bin::main("parrot-driver-t3", env!("CARGO_PKG_VERSION"), parrot_driver_t3::serve_stdio)
}
