//! Farpane Viewer: VNC-compatible remote desktop viewer.
//!
//! Placeholder entry point. The egui/eframe UI and the Tokio connection layer
//! come next (design doc sections 11 and 16).

fn main() {
    let theme = farpane_ui::ThemePreference::default().resolve(true);
    println!(
        "Farpane Viewer {} (protocol up to {:?}, theme {theme:?}) - not functional yet",
        env!("CARGO_PKG_VERSION"),
        rfb_proto::ProtocolVersion::V3_8,
    );
}
