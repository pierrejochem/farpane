---
title: Using Farpane Viewer
description: Connect to a VNC-compatible server with Farpane Viewer - sign in, trust a certificate, full screen, clipboard, keyboard capture, statistics and settings.
order: 3
---

# Using Farpane Viewer

Start `farpane-viewer` (optionally with `HOST:PORT`). The home screen has a **Quick connect** card for the address, a user name (only if the server asks) and a password (if the server asks), and a list of **Saved connections**. Press Enter or **Connect**.

## Signing in and trusting servers

The viewer prefers encrypted connections and never falls back to plain VNC Auth for a server you have already pinned.

- **VeNCrypt (TLS)**: the first time you see a server's certificate the viewer shows its SHA-256 fingerprint and asks whether to trust it. Your choice is remembered per address. If a known server's certificate changes, the viewer warns in red and the connection does not continue until you decide again.
- **RSA-AES**: the server's RSA key is pinned the same way, and this needs a user name and password.
- **VNC Auth**: the classic shared password, available for servers that offer nothing better. It is not encrypted.

Passwords are never stored and never taken from a command-line argument. For scripts, set `FARPANE_PASSWORD` or use `--password-file`.

## In a session

| Control | What it does |
|---|---|
| Full screen (F11) | Shows only the picture; move the pointer to the top edge for a pill with the controls. F11 is not sent to the remote machine. |
| Actual size / Fit to window | Switch between 1:1 and scaling to the window. |
| Ctrl+Alt+Del | Sends the key sequence to the remote machine. |
| Clipboard | Shares text both ways. With servers that support the Extended Clipboard extension it is UTF-8, otherwise Latin-1. |
| Resize remote | Asks the server to change its desktop size to match the window (when the server supports it). |
| Capture keys | Gives system shortcuts to the remote machine. Press and release Ctrl+Alt+Shift to hand the keyboard back. |
| Stats | Shows the Live stats card: round trip, rates, updates per second, the bandwidth chart and the encoding mix. Ctrl+Alt+Shift+S toggles it. |

The bar under the picture shows the protocol version, security type, picture size, extensions the server has shown and the pinned server key. Turn it off in **Settings**. In a narrow window the top bar folds the less used controls into a **More** menu.

## Settings

Settings live in `farpane/viewer.toml` in your configuration folder (`~/.config` or `$XDG_CONFIG_HOME` on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows). A damaged file is moved aside to `.broken`; a file written by a newer version is read but never overwritten.

Choose a theme (Glass, Slate, Light or System), an accent colour, the interface size (90, 100 or 115 percent), reduce transparency, and when to capture the keyboard (auto means full screen only).

## Headless use

```sh
farpane-viewer --screenshot out.png --encodings zrle,hextile,raw host:5900
```

saves one picture and prints which encoding was used. Add `--pin SHA256` to accept a certificate fingerprint and `--username NAME` for accounts.
