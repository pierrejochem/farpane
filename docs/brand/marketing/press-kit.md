# Farpane press kit

For anyone writing about Farpane. Facts here match the handbook and the
repository; if they disagree, the repository wins and this file is wrong.

## At a glance

| | |
|---|---|
| **Name** | Farpane (Farpane Viewer, Farpane Server) |
| **What** | VNC-compatible remote desktop viewer and server |
| **Tagline** | Far screens, close at hand. |
| **Status** | Early development, not yet released |
| **Licence** | MIT or Apache-2.0, at your option |
| **Language** | Rust (edition 2024, Rust 1.88 or newer) |
| **Maintainer** | Pierre Jochem |
| **Code** | https://github.com/pierrejochem/farpane |
| **Handbook** | https://pierrejochem.github.io/farpane/ |
| **Press and questions** | Open an issue on the repository |

## About

> Farpane is an open source remote desktop viewer and server that speaks the
> standard RFB protocol, so it connects to VNC-compatible software such as
> TigerVNC and QEMU. The server shares Linux desktops (X11, and Wayland through
> the desktop portal) and Windows desktops, including the lock screen through
> a service. Sign-in is encrypted with TLS (X.509, pinned fingerprints) or
> RSA-AES, against its own user list with Admin, Control and View roles. A
> dashboard shows clients and traffic; browsers connect through a WebSocket
> port. The protocol core is a small sans-IO Rust crate. Early development;
> the handbook lists what is verified and what is not.

More lengths (25, 50, 100 words) are in `copy.md`.

## Fact sheet

**Products**

- *Farpane Viewer*: egui window (Glass, Slate, Light and System themes), saved
  connections, certificate trust prompts, full screen with keyboard capture,
  live stats overlay, UTF-8 clipboard, optional H.264. Also a headless
  screenshot mode.
- *Farpane Server*: shares a desktop; dashboard window or headless service;
  first-run wizard; accounts, roles, lockout and per-client approval;
  `farpane-server user ...` for accounts on the command line.

**Protocol**

- RFB 3.3, 3.7 and 3.8, both roles.
- Encodings: Raw, CopyRect, RRE, CoRRE, Hextile, zlib, ZRLE, TRLE, Tight (no
  JPEG), and optional Open H.264. Pseudo-encodings: Cursor, DesktopSize,
  ExtendedDesktopSize, Fence, Extended Clipboard.
- Security types: None, VNC Auth, VeNCrypt (X509None, X509Vnc, X509Plain),
  RSA-AES (RA2, RA2_256), and Apple Remote Desktop sign-in in the viewer.
- Browser access through a WebSocket listener (`wss`), checked with noVNC 1.5.

**Platforms**

| | Viewer | Server |
|---|---|---|
| Linux (X11) | yes | yes |
| Linux (Wayland) | yes | yes, through the desktop portal; one monitor, no clipboard |
| Windows | builds and passes its tests | yes, as a service, including the lock screen; one picture over all monitors |
| macOS | builds and passes its tests; connects to macOS Screen Sharing | not built |

**Verified against**: TigerVNC 1.15, QEMU, macOS Screen Sharing (viewer), a
kiosk VeNCrypt server, noVNC 1.5 (server). **Not tested**: RealVNC products.

**Built with**: Rust, Tokio, rustls, egui and wgpu. Fonts: Inter and IBM Plex
Sans (SIL Open Font License).

## Trademark note

"VNC" is a trademark of RealVNC Ltd. Farpane is an independent project and not
affiliated with RealVNC. Please write "VNC-compatible", not "VNC".

## Images

All in `docs/brand/marketing/assets/` (rendered from `src/` by `render.py`):

| File | Size | Use |
|---|---|---|
| `github-social-preview.png` | 1280 x 640 | GitHub repository social preview |
| `og.png` | 1200 x 630 | Open Graph and link previews (also `site/static/og.png`) |
| `x-card.png` | 1600 x 900 | Post image for X, Mastodon, Bluesky |
| `square.png` | 1080 x 1080 | Square posts |
| `x-header.png` | 1500 x 500 | X profile header |
| `linkedin-banner.png` | 1584 x 396 | LinkedIn page banner |

Logos and icons are in `docs/brand/`:

| File | Use |
|---|---|
| `farpane-logo-dark.svg`, `farpane-logo-light.svg` | Lockup (mark and name) for dark and light backgrounds |
| `farpane-mark.svg`, `farpane-mark-mono.svg` | The mark, in colour and one colour |
| `farpane-viewer.svg`, `farpane-server.svg` (and `-1024.png`) | App icons |
| `packaging/icons/` | `.ico`, `.icns` and PNG sets |

Screenshots (real apps, Glass, Slate and Light): `site/static/screenshots/`.

Rules for using the mark are in `brand-guidelines.md`, section 5.
