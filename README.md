<p align="center"><img src="docs/brand/farpane-viewer-1024.png" width="128" alt="Farpane icon"></p>

# Farpane

A VNC-compatible remote desktop viewer and server, written in Rust.

- **Farpane Viewer** connects to any RFB/VNC server (TigerVNC, QEMU, macOS Screen Sharing, x11vnc, …).
- **Farpane Server** shares a Linux desktop (X11, or Wayland through the desktop portal) with its own user accounts, TLS or RSA-AES, and a dashboard window. Windows and macOS capture and input backends are not written yet.
- Encrypted by default for signed-in users (VeNCrypt with X.509, RSA-AES), Glass, Slate and Light themes. Browsers can connect through a WebSocket port (noVNC).

> Status: early development, not released. Viewer and server work against each other and against real servers (QEMU and a kiosk VNC server); see `CLAUDE.md` for what is verified and what is not.

Documentation: the [handbook](https://pierrejochem.github.io/farpane/handbook/) (source in `docs/handbook/`, built by `site/build.py` and published to GitHub Pages).

## Build

Requires Rust 1.88 or newer.

```sh
cargo build --workspace
cargo test --workspace
```

## Repository

| Path | Contents |
|---|---|
| `crates/rfb-proto` | Sans-IO RFB protocol core |
| `crates/rfb-codec` | Pixel encoders and decoders, clipboard messages |
| `crates/rfb-security` | Cryptography for the security types (TLS trust, RSA-AES, VNC Auth) |
| `crates/rfb-tokio` | Async client and server sessions |
| `crates/rfb-server` | The server engine: capture thread, per-client tasks, `Source` and `Input` traits |
| `crates/farpane-x11`, `crates/farpane-wayland` | Linux capture and input backends |
| `crates/farpane-grab` | Keyboard capture for the viewer |
| `crates/farpane-ui` | Shared dashboard UI and themes |
| `apps/` | `farpane-viewer` and `farpane-server` binaries |
| `packaging/` | Installers and packages for Windows, macOS and Linux |
| `docs/handbook/` | The handbook (Markdown), published as the project site |
| `site/` | Landing page generator, styles and social preview for the site |
| `docs/design/` | Design reference and UI mockups |
| `docs/brand/` | Icon and brand assets |
| `tools/interop/` | Independent RSA-AES implementation in Python, used to cross-check the Rust one |

## Releases

Pushing a tag `vX.Y.Z` builds packages for Windows (MSI, zip), macOS (DMG) and Linux (deb, rpm, Arch, AppImage, tarball) and attaches them to a draft GitHub Release. See `packaging/README.md`.

## Licence

Dual-licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option. The embedded fonts (Inter, IBM Plex Sans) are under the SIL Open Font License; their licences are in `crates/farpane-ui/assets/fonts/`.

"VNC" is a trademark of RealVNC Ltd. Farpane is an independent project and not affiliated with RealVNC.
