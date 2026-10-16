# Farpane – guide for Claude Code

Farpane is a VNC-compatible remote desktop viewer and server written in Rust.
Two apps, **Farpane Viewer** (`farpane-viewer`) and **Farpane Server**
(`farpane-server`), share a sans-IO protocol core and an egui dashboard UI.

The full design lives in `docs/design/farpane-reference.md` (exported
2026-10-16 from the shared design doc). Read the section you need before
changing a subsystem; section numbers below refer to it.

## Layout

| Path | What |
|---|---|
| `crates/rfb-proto` | Sans-IO RFB core, `no_std + alloc`, depends only on `bytes`. Parsing, serialising, rectangle framing, handshake state machines. No sockets, async, crypto or pixel decoding here (§15). |
| `crates/rfb-codec` | Pixel encoders/decoders (Raw, CopyRect, Hextile, ZRLE, Tight, H.264). Gets complete rect payloads from `rfb-proto` (§7, §15, §22). |
| `crates/rfb-security` | Security handshakes behind the `SecurityStep` trait: None, VNC Auth, VeNCrypt (rustls), RA2/RA2-256, Apple DH (§4, §20). |
| `crates/farpane-ui` | Shared egui UI: design tokens, themes, widgets, charts (§16). Widgets read tokens only, never raw colours. |
| `apps/farpane-viewer`, `apps/farpane-server` | Thin binaries over the crates. |
| `packaging/` | nfpm (deb/rpm/Arch), WiX MSI, macOS plists, icons. See `packaging/README.md`. |
| `.github/workflows/` | `ci.yml` (fmt, clippy, test), `release.yml` (signed packages to a draft GitHub Release on tag `vX.Y.Z`). |
| `docs/design/` | Design reference, UI mockup sources, glass backdrop. |
| `docs/brand/` | Icon masters (concept 03 "Receding panes"). |

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets   # must stay warning-free (pedantic is on)
cargo fmt --all
```

## Decisions already made (do not reopen without asking)

- Name: Farpane. Never put "VNC" in a product or crate name (RealVNC trademark); say "VNC-compatible" in text.
- `rfb-proto` is sans-IO and runtime-independent; Tokio adapter first. It uses `bytes`, does rectangle framing itself, and supports RFB 3.3, 3.7 and 3.8 in both roles.
- Every error except `ProtoError::NeedMore` is fatal. Bound every length field (limits in `rfb_proto::consts::limits`).
- Server: hybrid concurrency (one capture task broadcasting damage, one task per client owning its encoder state) (§23).
- Encoded-tile caching later, stateless encodings only (§23).
- Unattended access: system service + per-session worker, login screen where the platform allows; Linux also virtual headless sessions (§19).
- Server authenticates against its own user list (Argon2id), roles Admin / Control / View only (§20).
- Own file-transfer extension needs IANA-registered numbers before release (§21).
- Browser access in v1: WebSocket listener, `wss`, noVNC-compatible (VeNCrypt-Plain and RA2ne on that listener) (§21).
- H.264: encoding 50, Constrained Baseline, OS hardware encoders first, OpenH264 (downloaded from Cisco) as fallback, off by default (§22).
- UI: egui/eframe on wgpu. Themes Glass (default), Slate, Light, System; options accent, reduce transparency, interface size, reduce motion (§16).
- Platforms for the first server release: Linux Wayland, Linux X11, macOS, Windows.

## Conventions

- Protocol numbers come only from `rfb_proto::consts`; they are verified against IANA. Add new ones there with a source.
- New wire messages get byte-exact golden tests, a round-trip test and a split-input test (feed byte by byte).
- `unsafe` is denied workspace-wide; platform code that needs it goes in its own crate with a written justification.
- Strings on the wire: send UTF-8, accept invalid UTF-8 without losing sync.

## Where we are

Milestone M1 (protocol skeleton) is done: `consts`, `version`, `types`, `wire::Reader`, `msg::{client,server}` (core messages, golden + split-input tests) and `handshake` (client and server machines for 3.3/3.7/3.8, None and VNC Auth; VNC Auth is a caller-answered challenge; `rfb-security::vnc_auth` computes and verifies the DES response; proptest round-trips cover the messages).

Next steps, in order:
1. M1 is done apart from optional `wire::Writer`.
2. M2: `framing::frame` is done for Raw, CopyRect, RRE, CoRRE, Hextile, zlib, ZRLE, Cursor, DesktopSize, LastRect. `conn::ClientConn`/`ClientConnEvent` (handshake + server-message loop, bounds checks, DesktopSize, LastRect, zero-copy `Bytes` rect payloads) is done. `conn::ServerConn` is done too (client-message loop; `send_update` checks encoding was requested, bounds and that the payload frames to its own length). Fuzz targets live in `crates/rfb-proto/fuzz` (`client_conn`, `server_conn`, `framing`, `messages`; own workspace, need nightly + `cargo install cargo-fuzz`, run with `cargo +nightly fuzz run <target>` from `crates/rfb-proto`); each ran 60 s on 2026-10-16 with no findings (7.6M, 9.4M, 0.4M and 0.65M execs for messages, framing, client_conn, server_conn). Stable robustness proptests in `conn/tests.rs` cover the same ground. Still open: TRLE (no section in rfbproto.rst, needs a verified source first), a fuzz job in CI. `limits::MAX_RECT_PAYLOAD` (128 MiB) is our own bound, not from the spec.
3. Tokio adapter and a minimal viewer that connects to a TigerVNC or QEMU server.

## Open items

- Secure the name: EU trademark (classes 9, 42), placeholder crates, GitHub organisation, domain.
- Licence, maintainer, homepage and bundle-ID domain are still TODO (`Cargo.toml`, `packaging/`).
- Minimum OS versions (macOS 12.3+, glibc 2.34) are assumptions.
- Signing accounts (Apple Developer, Azure Artifact Signing) come later; releases are unsigned until then.
- Windows `.exe` icons need a `build.rs` with the `winresource` crate.

## Links

- Design doc (live, editable): https://claude.ai/code/artifact/6f65eb71-d5ea-4de9-9a8c-b24bc013b480
- UI and icon mockups (Design canvas): https://claude.ai/artifact/9E9g4hrbyjJAuM9JuRDXVN
- RFB community spec: https://github.com/rfbproto/rfbproto/blob/master/rfbproto.rst
- RFC 6143: https://www.rfc-editor.org/rfc/rfc6143
- IANA RFB registry: https://www.iana.org/assignments/rfb/rfb.xhtml
