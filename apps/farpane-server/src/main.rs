//! Farpane Server: VNC-compatible remote desktop server.
//!
//! Placeholder entry point. `--headless` (no dashboard window) is already
//! referenced by `packaging/linux/farpane-server.service`.

fn main() {
    let headless = std::env::args().any(|a| a == "--headless");
    println!(
        "Farpane Server {} ({}) - not functional yet",
        env!("CARGO_PKG_VERSION"),
        if headless { "headless" } else { "dashboard" },
    );
}
