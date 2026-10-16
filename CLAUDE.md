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
| `crates/rfb-tokio` | Tokio adapter: `ClientSession` / `ServerSession` over any `AsyncRead + AsyncWrite`. I/O, 30 s handshake timeout, VNC Auth via `rfb-security`; no policy. The richer `rfb-client` / `rfb-server` layers of §11 will sit on top. |
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
3. Tokio adapter is done (`rfb-tokio`, loopback + TCP tests). `rfb-codec` has the 32 bpp `Framebuffer` (B,G,R,0xFF), `PixelConverter` (any true-colour format; colour-mapped is unsupported) and a `Decoder` for Raw, CopyRect, RRE, CoRRE, Hextile, DesktopSize (Hextile accepts the spec's invalid colour carry-overs like TigerVNC, errors only on a never-specified colour). zlib and ZRLE decode too (`flate2` with the pure-Rust backend; one persistent inflate stream per encoding inside `Decoder`, so rects must be decoded in arrival order; inflated size is capped per rect as a zip-bomb guard). Still open: TRLE.

`apps/farpane-viewer` has a library (`RemoteScreen`: connect, decode, keep the picture) and a headless CLI: `farpane-viewer --screenshot out.png|out.ppm [--encodings zrle,hextile,zlib,corre,copyrect,raw] host:port` (password via `$FARPANE_PASSWORD` or `--password-file`, never an argument). Verified against a real QEMU VNC server (`apps/farpane-viewer/tests/qemu.rs`, skips without `qemu-system-x86_64`): Raw, Hextile, zlib and ZRLE render the same BIOS screen, a mid-session DesktopSize works, VNC Auth works, wrong or missing passwords fail cleanly. Not yet tested against TigerVNC. 

The viewer also has an egui window (`farpane-viewer [HOST:PORT]`, `app::ViewerApp`): a home card to connect (address, password), the remote picture as a texture with partial uploads, Fit / Actual size, a full-screen button, theme switch (Glass, Slate, Light, System via `farpane_ui::style`), keyboard and pointer forwarding (`input.rs`: egui events to X11 keysyms, modifier tracking, buttons, wheel, letterboxed mapping, release on focus loss) and a connection thread (`core.rs`, `Command` / `UiEvent` channels). `--ui-screenshot FILE` makes the window save a screenshot of itself and quit, which is how it is checked here (no OS input injection available). `eframe` is used with `egui-wgpu` default features added explicitly, otherwise no GPU backend is compiled in. Tests: `tests/ui.rs` drives the app with synthetic egui input against an in-process server. The remote cursor works: `rfb-codec` decodes the Cursor pseudo-encoding (`Damage::Cursor`, hotspot, RGBA with a 1-bit mask), the window draws the shape at the pointer and hides the OS cursor (an empty shape hides it entirely; until a shape arrives the local arrow stays). Only tested against in-process servers: QEMU in BIOS text mode never sends a shape, so it is unverified against a real server. Clipboard sync (legacy Latin-1 text) works in both directions: on Ctrl/Cmd+V the window sends the local text as `ClientCutText` and then taps Ctrl+V for the remote machine; Ctrl+C and Ctrl+X are taps too, and the server's `ServerCutText` goes to the local clipboard (control characters other than tab and newline are dropped, so a remote cannot slip escape sequences into a later terminal paste). egui-winit turns those shortcuts into `Event::Copy/Cut/Paste` and swallows the key, so the tap is synthesised in `KeyTranslator`. A top-bar Clipboard checkbox turns the sharing off (the shortcut keys still work). Limits: non-Latin-1 characters become `?` (the UTF-8 Extended Clipboard is M3), over 1 MiB is not sent, Ctrl+V with an empty or non-text local clipboard sends nothing, Cmd+C on macOS reaches the remote as Super+C. Full screen: F11 or the Full screen button (`--fullscreen` starts that way); only the picture shows, with a grip tab at the top edge; the top edge or a click on the tab slides down an opaque pill (host and size, Ctrl+Alt+Del, Actual size, Clipboard, Pin, Exit full screen, Disconnect) that hides 2 s after the pointer leaves unless pinned. F11 is not sent to the remote machine, clicks on the pill do not reach it, and losing the connection leaves full screen. Timers use egui's clock, so tests drive them. Settings and address book: `settings.rs` (TOML `farpane/viewer.toml` in the platform config dir: `$XDG_CONFIG_HOME` or `~/.config`, `~/Library/Application Support`, `%APPDATA%`; atomic 0600 writes; a damaged file is moved to `.broken`; a file with a newer `version` is read but never saved over; unknown keys ignored) and `appearance.rs` (strings to theme, accent, interface size 90/100/115 %, reduce transparency). The window loads them at start, applies them (also zoom), saves changes with the next frame, remembers each successful connection (name from the server, deduplicated, newest first, max 50) and shows them as cards on the home screen (Connect, × to forget). A Settings window sits in the top bar. `Startup.settings_path` is `None` in tests, so tests never touch your config. Passwords are not stored (OS keychain is open). Stats overlay: toggle with the top-bar or toolbar Stats button, Ctrl+Alt+Shift+S (handled locally, not sent to the remote) or `--stats`. It shows updates per second, in and out rates, the pixel-encoding mix by bytes (pseudo-encodings excluded), size, totals and a 60 s bandwidth sparkline (`farpane_ui::widgets::sparkline`). Readings come from `stats::Collector` (fed by `RemoteScreen` and `rfb-tokio`'s `bytes_received/bytes_sent`) via `UiEvent::Stats` every 250 ms. There is deliberately no round-trip time: that needs the Fence extension, and a figure derived from update requests would only measure how long the screen stayed unchanged. VeNCrypt TLS (security type 19): `rfb-proto` negotiates 0.2 for both roles (sans-IO: `StartTls` event, then `tls_established()`; only X509None, X509Vnc and X509Plain are ever chosen; plaintext buffered when TLS starts is refused); `rfb-security::tls` holds the trust policy (accept a pinned SHA-256 fingerprint, or a certificate a public or supplied CA signed for the name, otherwise fail and report unknown / changed / invalid with the fingerprint); `rfb-tokio` upgrades the `Transport` in place (client with `TlsOptions`, server with a rustls `ServerConfig`, X509Plain via `PlainVerifier`; without TLS options a client never picks VeNCrypt). The viewer asks on first sight (trust prompt with the fingerprint, red warning when a known server's certificate changed), remembers the choice as `[[known_host]]` in the settings (a new pin replaces the old one), and has a Username field for X509Plain; headless `--pin SHA256` and `--username`. Verified against QEMU's gnutls VeNCrypt server (untrusted then pinned, changed, private CA, X509Vnc with a password), and against a recording relay that shows the clipboard text, password and desktop name never appear on the wire under TLS. Not supported: anonymous TLS subtypes (rustls cannot do them, and they are open to a man in the middle), Plain without TLS, SASL, Ident, client certificates, the OS trust store (public CAs come from `webpki-roots`; a private CA needs `extra_roots` or a pin), certificate revocation. Keyboard capture: `crates/farpane-grab` (the only crate allowed `unsafe`, each block justified) asks the windowing system to give the window system shortcuts: Wayland via `zwp_keyboard_shortcuts_inhibit_manager_v1` on winit's own connection (confirmed `active` only once the window has focus), X11 via Xlib `XGrabKeyboard` on winit's own `Display*` (a grab is delivered to the grabbing client, so it must be the window's own; a temporary error handler keeps a `BadWindow` from exiting the process; refusals such as "not viewable" during the full-screen transition are retried every 500 ms). Windows and macOS are Unsupported (cannot be tested here). The viewer (`capture.rs`) captures when connected, focused, not released by the user, and the preference allows (`auto` = full screen only, `always`, `never`; setting `[input] capture`); Ctrl+Alt+Shift pressed and released with no other key gives the keyboard back (so the Ctrl+Alt+Shift+S stats hotkey is safe), a click takes it again; a Capture keys toggle sits in the top bar and toolbar. Verified: X11 grab exclusivity and release against the real X server, Wayland requests accepted by GNOME's mutter, and both backends engaged from the real window (X11 went Active after one retry). Not verified: that shortcuts really reach the remote machine (needs a focused window and a person; this XWayland does not deliver injected keys even without a grab, so that check skips itself). Known limits: the Super/Windows key has no egui representation, so it cannot be forwarded; under XWayland on this machine wgpu's default backend fails (`Surface::configure`), `WGPU_BACKEND=vulkan` works; GNOME does not give focus to a window started from a non-interactive process, so a scripted launch never captures. ExtendedDesktopSize: `rfb-proto` has `Screen`, the `SetDesktopSize` message, the rectangle framer and bounds tracking (the size changes only when the status is 0, since a refusal's size fields are undefined; `ClientConn` refuses to send `SetDesktopSize` until it has seen an `ExtendedDesktopSize` rectangle); `rfb-codec` decodes it to `Damage::Layout` (`resized` only when the size really changed, since servers repeat the layout on every full refresh); the viewer asks for both size encodings and has a Resize remote option (top bar, toolbar; saved as `[input] resize_remote`) that asks for the window's pixel size once it has been stable for 0.4 s, never asks twice for the same size (so a server that picks another size cannot cause a loop), and stops after a refusal until toggled. One screen is requested, keeping the first screen's id and flags. Against real QEMU: the layout is announced and matches the framebuffer, and a resize request is answered (status 3, invalid layout, for the BIOS guest). Not built yet: multi-monitor layouts, RTT (Fence), keyboard capture in full screen (system shortcuts still go to the local OS; per-platform grab and the release chord are open), the double-click-title toggle, multi-monitor spanning, stats overlay, address book, saved settings, Glass blur backdrop, bundled fonts (Inter, IBM Plex Sans), real-OS input checks on X11, macOS and Windows.

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
