//! Embeds the icon and version information into the Windows executable, so
//! Explorer, the taskbar and the Services list show them. Does nothing on
//! other targets.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/icons/farpane-server.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../packaging/icons/farpane-server.ico")
        .set("ProductName", "Farpane")
        .set(
            "FileDescription",
            "Farpane Server: VNC-compatible remote desktop server",
        )
        .set("CompanyName", "Pierre Jochem")
        .set("LegalCopyright", "MIT OR Apache-2.0");
    if let Err(e) = res.compile() {
        println!("cargo:warning=cannot embed the Windows icon: {e}");
    }
}
