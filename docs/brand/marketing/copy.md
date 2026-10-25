# Farpane copy deck

Ready-to-use text. Every claim here is checked against the claims policy in
`brand-guidelines.md`. While Farpane is unreleased, keep the status line.

**Status line:** *Early development, not yet released.*

## Names and lines

| Use | Text |
|---|---|
| Name | Farpane |
| Tagline | Far screens, close at hand. |
| Descriptor | VNC-compatible remote desktop viewer and server. Open source, written in Rust. |
| Short descriptor (under 60 chars) | VNC-compatible remote desktop in Rust |
| GitHub "About" (under 350 chars) | VNC-compatible remote desktop viewer and server in Rust. Encrypted sign-in (TLS, RSA-AES), accounts with roles, Wayland and X11, Windows service, browser access. MIT or Apache-2.0. |
| GitHub topics | `vnc`, `rfb`, `remote-desktop`, `rust`, `wayland`, `x11`, `self-hosted`, `egui`, `tokio`, `novnc` |
| Store-style subtitle | Remote desktop that stays yours |

Alternative taglines, if the primary does not fit:

- Your screens, wherever they are.
- Open protocol. Encrypted sign-in. No middleman.
- The remote desktop you run yourself.

## Boilerplate

**25 words**

> Farpane is an open source, VNC-compatible remote desktop viewer and server written in Rust. It adds encrypted sign-in, user accounts and a dashboard to the standard RFB protocol.

**50 words**

> Farpane is a VNC-compatible remote desktop viewer and server, written in Rust and licensed MIT or Apache-2.0. It speaks standard RFB, so it works with the software people already run, and adds encrypted sign-in (TLS or RSA-AES), accounts with roles, a dashboard, and a Wayland backend.

**100 words**

> Farpane is an open source remote desktop viewer and server that speaks the standard RFB protocol, so it connects to VNC-compatible software such as TigerVNC and QEMU. The server shares Linux desktops (X11, and Wayland through the desktop portal) and Windows desktops, including the lock screen through a service. Sign-in is encrypted with TLS (X.509, pinned fingerprints) or RSA-AES, against its own user list with Admin, Control and View roles. A dashboard shows clients and traffic; browsers connect through a WebSocket port. The protocol core is a small sans-IO Rust crate. Early development; the handbook lists what is verified and what is not.

Always followed, where space allows, by: *"VNC" is a trademark of RealVNC Ltd.
Farpane is an independent project and not affiliated with RealVNC.*

## Feature lines

One idea each, for cards, slides and lists.

- **Connects to what you run.** Viewer verified against TigerVNC 1.15, QEMU and macOS Screen Sharing.
- **Encrypted sign-in.** VeNCrypt over TLS with pinned fingerprints, or RSA-AES.
- **Your own accounts.** Argon2id hashes, Admin, Control and View roles, lockout after repeated failures.
- **Approve each client.** Hold a connection until someone at the machine says yes.
- **Wayland and X11.** The Wayland backend uses the desktop portal and PipeWire.
- **Windows service.** Unattended access that follows the console session, down to the lock screen.
- **Browser access.** A `wss` port that noVNC connects to.
- **A dashboard, not a log file.** Clients, traffic, events and settings in one window.
- **Fast enough to measure.** ZRLE, Tight and optional H.264, and a stats overlay with the real numbers.
- **Readable.** Rust, a sans-IO protocol core, and a handbook that says what is verified.

## Launch posts

Post only after a release exists, or keep the status line in each. Replace
`<link>` and `<version>`.

### Show HN

**Title:** Show HN: Farpane – VNC-compatible remote desktop viewer and server in Rust

> I wanted a remote desktop I could run myself that had encrypted sign-in,
> real user accounts and a Wayland story, without leaving the VNC protocol
> everything already speaks. Farpane is a viewer and a server, MIT or
> Apache-2.0.
>
> What works: the viewer connects to TigerVNC 1.15, QEMU and macOS Screen
> Sharing; the server shares X11 and Wayland (portal) desktops on Linux and a
> Windows desktop through a service, signs users in over TLS or RSA-AES, and
> serves browsers through a WebSocket port (checked with noVNC).
>
> What does not: no macOS server yet, no Wayland clipboard, no hardware H.264
> encoders. The handbook lists what is verified and what is not.
>
> The protocol core is a sans-IO crate (`no_std + alloc`) that you can read in
> an afternoon. Feedback on the protocol corners (Apple's auth, Tight, the
> Extended Clipboard) is especially welcome.
>
> <link>

### Reddit: r/rust

**Title:** Farpane: a VNC-compatible remote desktop viewer and server, with a sans-IO RFB core

> Sharing a project I have been building: a remote desktop viewer and server
> in Rust. The interesting part for this sub may be the layering: a sans-IO
> RFB core (`rfb-proto`, `no_std + alloc`, depends only on `bytes`), pixel
> codecs, a crypto crate with no I/O, and a Tokio adapter on top. The UI is
> egui on wgpu.
>
> Everything is checked against real servers where I could (TigerVNC, QEMU,
> noVNC in headless Chrome). The README and handbook say what is not.
>
> <link>

### Reddit: r/selfhosted

**Title:** Farpane: self-hosted remote desktop that speaks VNC, with accounts, TLS and a dashboard

> If you run x11vnc or TigerVNC and wish it had user accounts, encrypted
> sign-in and a Wayland backend, this might be for you. Farpane is a viewer
> and a server, open source, no cloud account and no relay: you connect
> straight to your machine.
>
> The server has a dashboard (clients, traffic, events), approval of new
> clients, and a first-run wizard. Browsers can connect with noVNC. Early
> development: Linux and Windows servers work, a macOS server does not exist
> yet.
>
> <link>

### X / Mastodon thread

1. Farpane is a VNC-compatible remote desktop viewer and server, open source, written in Rust. Far screens, close at hand. <link>
2. It speaks standard RFB, so the viewer connects to TigerVNC, QEMU and macOS Screen Sharing, and the server works with other viewers and with noVNC in a browser.
3. Sign-in is encrypted: TLS with pinned fingerprints, or RSA-AES. Accounts have Admin, Control or View roles, hashed with Argon2id.
4. The server shares Linux desktops (X11, and Wayland through the desktop portal) and Windows desktops, including the lock screen, through a service.
5. Not built yet: a macOS server, Wayland clipboard, hardware H.264 encoders. The handbook lists what is verified. Early development.

### LinkedIn

> Remote desktop is usually either a hosted service or an unencrypted
> protocol from the 1990s. Farpane is a third option: the VNC protocol, with
> encrypted sign-in, user accounts and a dashboard, in an open source viewer
> and server written in Rust. You connect straight to your own machine, with
> no vendor account in between. Early development; the handbook lists what is
> verified. <link>

### Release notes template

```
## Farpane <version>

One sentence on what changed for someone who uses it.

### Added
- 

### Changed
- 

### Fixed
- 

### Not yet
- (carry over what is still unbuilt; keep this section honest)

Checksums and signatures: see the release assets. Packages are unsigned
until signing accounts exist.
```

## Short answers (FAQ voice)

- **Is it VNC?** It speaks the same protocol (RFB), so VNC-compatible software
  connects to it. The name avoids "VNC" because that is RealVNC's trademark.
- **Why not a hosted service?** Farpane has no relay and no account with us.
  You connect to your own machine on your own network or tunnel.
- **Is it secure?** Sign-in is encrypted (TLS or RSA-AES) and checked against
  hashed passwords, with lockout. It is early software and has not been
  audited. Say that.
- **Does it work on a Mac?** The viewer builds on macOS and connects to macOS Screen Sharing. The macOS
  server (capture and input) is not written yet.
- **What does it cost?** Nothing. MIT or Apache-2.0, your choice.
