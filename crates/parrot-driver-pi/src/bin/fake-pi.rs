//! `fake-pi`: a pi RPC CLI for tests, driven by the tags in a prompt.

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime builds");
    if let Err(e) = runtime.block_on(parrot_driver_pi::fake::run()) {
        eprintln!("fake-pi: {e}");
        std::process::exit(1);
    }
}
