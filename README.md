<p align="center"><img src="docs/brand/farpane-viewer-1024.png" width="128" alt="Farpane icon"></p>

# Farpane

A VNC-compatible remote desktop viewer and server, written in Rust.

- **Farpane Viewer** connects to any RFB/VNC server (TigerVNC, QEMU, macOS Screen Sharing, x11vnc, …).
- **Farpane Server** shares Windows, macOS and Linux (X11 and Wayland) desktops, including unattended access, with its own user accounts and a dashboard UI.
- Modern security by default (VeNCrypt with X.509, RA2-256), browser access via noVNC, Glass, Slate and Light themes.

> Status: early development. The protocol core (`crates/rfb-proto`) is being built; the apps are placeholders.

## Build

Requires Rust 1.85 or newer.

```sh
cargo build --workspace
cargo test --workspace
```

## Repository

| Path | Contents |
|---|---|
| `crates/rfb-proto` | Sans-IO RFB protocol core |
| `crates/rfb-codec` | Pixel encodings |
| `crates/rfb-security` | Security handshakes |
| `crates/farpane-ui` | Shared dashboard UI and themes |
| `apps/` | `farpane-viewer` and `farpane-server` binaries |
| `packaging/` | Installers and packages for Windows, macOS and Linux |
| `docs/design/` | Design reference and UI mockups |
| `docs/brand/` | Icon and brand assets |

## Releases

Pushing a tag `vX.Y.Z` builds packages for Windows (MSI, zip), macOS (DMG) and Linux (deb, rpm, Arch, AppImage, tarball) and attaches them to a draft GitHub Release. See `packaging/README.md`.

## Licence

To be decided (planned: MIT OR Apache-2.0).

"VNC" is a trademark of RealVNC Ltd. Farpane is an independent project and not affiliated with RealVNC.
