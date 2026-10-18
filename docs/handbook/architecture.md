---
title: Architecture
description: How Farpane is built - the sans-IO protocol core, codecs, security, Tokio adapter, server engine, platform backends and the shared egui interface.
order: 8
---

# Architecture

Farpane is a Rust workspace of small crates with one direction of dependency: applications on top, then the server engine and UI, then the Tokio adapter, then the pure protocol crates.

| Crate | Role |
|---|---|
| `rfb-proto` | Sans-IO RFB core (`no_std` plus `alloc`, depends only on `bytes`): parsing, serialising, rectangle framing and handshake state machines. No sockets, async, crypto or pixel decoding. |
| `rfb-codec` | Pixel decoders and encoders, the framebuffer and pixel-format conversion, clipboard messages. |
| `rfb-security` | Cryptography for the security types without I/O: VNC Auth, the TLS trust policy, RSA-AES. |
| `rfb-tokio` | `ClientSession` and `ServerSession` over any `AsyncRead + AsyncWrite`: I/O, handshake timeout, TLS upgrade, RSA-AES. Policy-free. |
| `rfb-server` | Server engine: one capture thread broadcasting damage, one task per client owning its encoder state, plus Fence pacing, scroll copies, the client registry and throttling. |
| `farpane-x11`, `farpane-wayland` | Linux capture and input backends behind the `Source` and `Input` traits. |
| `farpane-grab` | Viewer keyboard capture; allowed `unsafe`, each block justified. |
| `farpane-win` | Windows capture (GDI) and input (`SendInput`, clipboard) behind the `Source` and `Input` traits, plus the session worker: the service starts the worker in the console session and `remote` forwards screen and input over pipes. The wire format and the forwarding are plain Rust and tested on any system. |
| `farpane-winsvc` | Windows service host (service control manager entry point, install, uninstall); empty on other systems. |
| `farpane-ui` | Design tokens, themes, widgets, icons and charts shared by both apps. |
| `farpane-viewer`, `farpane-server` | The applications. |

## Design decisions

- **Sans-IO core**: the protocol logic runs on byte slices and returns events, so it is testable without a network and independent of any async runtime. Every error except "need more input" is fatal, and every length field is bounded.
- **Server concurrency**: one capture task broadcasts damage; each client task owns its dirty region and encoder streams, so a slow client skips frames instead of slowing the others.
- **One UI toolkit**: egui on wgpu, with design tokens read by every widget, so the Glass, Slate and Light themes and the accent options apply everywhere.
- **Tests**: byte-exact golden tests, round-trip and split-input tests for every wire message, proptest and fuzz targets for parsers, and in-process end-to-end tests of the apps.

The design reference lives in `docs/design/` in the repository.
