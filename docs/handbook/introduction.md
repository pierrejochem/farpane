---
title: Introduction
description: What Farpane is - a VNC-compatible remote desktop viewer and server written in Rust - what it can do today and what is not built yet.
order: 1
---

# Introduction

Farpane is a **VNC-compatible remote desktop viewer and server written in Rust**. It speaks the RFB protocol (versions 3.3, 3.7 and 3.8), so the viewer connects to servers such as TigerVNC, QEMU and x11vnc, and the server accepts connections from other VNC viewers.

There are two applications:

- **Farpane Viewer** (`farpane-viewer`) is a desktop window that shows a remote screen and forwards keyboard, pointer and clipboard.
- **Farpane Server** (`farpane-server`) shares a Linux desktop (X11, or Wayland through the desktop portal) with its own user accounts, encryption and a dashboard window.

Both share a protocol core written without any I/O (`rfb-proto`) and an egui user interface with the **Glass**, **Slate** and **Light** themes.

![Farpane Viewer showing a remote desktop with the Live stats card and the footer bar](screenshots/viewer-session.png){: width="1100" height="720" loading="lazy"}

![Farpane Server dashboard overview with live figures, traffic chart, shared screen and security card](screenshots/server-overview.png){: width="1100" height="760" loading="lazy"}

## Status

Farpane is in early development and has **not been released**. The viewer and server work against each other and against real servers (QEMU, TigerVNC and a kiosk VNC server). It has not yet been tested against RealVNC or macOS Screen Sharing, so compatibility with those is expected but unverified.

| Area | State |
|---|---|
| Viewer on Linux, Windows, macOS | Builds and passes tests on all three. Keyboard capture works on X11 and Wayland only. |
| Server on Linux (X11, Wayland) | Works. |
| Server on Windows and macOS | Capture and input backends are not written yet. |
| Encodings | Raw, CopyRect, RRE, CoRRE, Hextile, zlib, ZRLE, TRLE and Tight (no JPEG), and Open H.264 through Cisco's OpenH264 library (opt-in, downloaded on request). |
| Browser access (noVNC) | A WebSocket port (`--ws`), checked against noVNC 1.5.0 in Chrome. |

The [protocol support](protocol.html) page lists exactly what is verified, and the [security model](security.html) explains how connections are protected.

## Name and trademark

"VNC" is a trademark of RealVNC Ltd. Farpane is an independent project and not affiliated with RealVNC; that is why the products say "VNC-compatible" and never put "VNC" in a name.

## Licence

Farpane is dual-licensed under the Apache License 2.0 or the MIT licence, at your option. The source is on [GitHub](https://github.com/pierrejochem/farpane).
