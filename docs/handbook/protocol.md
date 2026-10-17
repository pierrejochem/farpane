---
title: Protocol support
description: Which RFB protocol versions, security types, encodings and extensions Farpane supports, and which servers it has been tested against.
order: 6
---

# Protocol support

Farpane implements the RFB protocol as described in RFC 6143 and the community specification (`rfbproto`). All protocol numbers are checked against the IANA registry.

## Versions

RFB 3.3, 3.7 and 3.8, in both roles (viewer and server).

## Security types

| Type | Viewer | Server |
|---|---|---|
| None | yes | yes |
| VNC Auth | yes | yes |
| VeNCrypt (X509None, X509Vnc, X509Plain) | yes | yes |
| RSA-AES (RA2, RA2_256, RA2ne, RA2ne_256) | yes (the `ne` types are never chosen by default) | yes |
| SASL, Apple DH, MS-Logon II, RA2r | no | no |

## Pixel encodings

| Encoding | Decode (viewer) | Encode (server) |
|---|---|---|
| Raw, CopyRect | yes | yes (CopyRect for vertical and sideways scrolls) |
| RRE, CoRRE | yes | no |
| Hextile | yes | yes |
| zlib | yes | no |
| ZRLE | yes | yes |
| TRLE | yes | yes |
| Tight (no JPEG) | yes | yes |
| Open H.264 (50) | yes, with Cisco's OpenH264 library | yes, with Cisco's OpenH264 library |

Tight JPEG is refused because the viewer never asks for a JPEG quality level, which the specification requires before a server may send it.

## Pseudo-encodings and extensions

Cursor, DesktopSize, ExtendedDesktopSize (the viewer can ask a server to resize), Fence (round-trip measurement and pacing), Extended Clipboard (UTF-8 text) and LastRect.

## Verified against

- **QEMU's VNC server**: Raw, Hextile, zlib, ZRLE and Tight; VNC Auth; VeNCrypt with X509None and X509Vnc; a mid-session resize.
- **A real kiosk VNC server** (VeNCrypt X509Vnc with a self-signed certificate): Raw, ZRLE, Tight, Hextile, zlib, CoRRE and RRE.
- **Farpane against itself**, end to end, including TRLE and RSA-AES.

Not yet tested against TigerVNC, RealVNC or macOS Screen Sharing.
