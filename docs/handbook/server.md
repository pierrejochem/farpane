---
title: Running Farpane Server
description: Share a Linux desktop (X11 or Wayland) with Farpane Server - user accounts, TLS, RSA-AES, the dashboard, settings and running it as a service.
order: 4
---

# Running Farpane Server

`farpane-server` shares a screen and opens a dashboard window. Pass `--headless` to run without the window, for example as a service.

## What to share

| Option | Shares |
|---|---|
| (none) | An animated test pattern, for trying things out. |
| `--x11 [DISPLAY]` | An X11 display. |
| `--wayland` | A Wayland desktop through the desktop portal. The desktop asks for approval once; the approval is remembered in the config folder. |
| `--desktop` | The desktop of the session the server runs in: Wayland if `WAYLAND_DISPLAY` is set, else X11. |

Under the rootless XWayland of a Wayland desktop, capturing the X11 root fails, so use `--wayland` there.

## Who may connect

By default the server listens on `127.0.0.1:5900` and refuses a non-loopback address unless there are credentials (or you pass `--allow-no-auth`, which is a bad idea without a tunnel).

- **Accounts** (recommended): `farpane-server user add NAME` creates a user. The first user is an administrator. Roles are `admin`, `control` and `view` (view looks only). Passwords come from `FARPANE_PASSWORD`, `--password-file` or standard input, never from arguments, and are 8 to 255 bytes. Other commands: `passwd`, `role`, `disable`, `enable`, `remove`, `list`. The last enabled admin cannot be removed without `--force`.
- **Shared password**: `--password-file FILE` for VNC Auth when there are no accounts.

Accounts sign in by name and password inside encryption: TLS (`--tls-self-signed` or `--tls-cert` and `--tls-key`) or RSA-AES (`--ra2`). The server prints the certificate and RSA key fingerprints at start; give them to your users so they can compare them in the viewer.

## Approving connections

With `--require-approval` (or **Ask before a client gets in** on the Security page) every client that signs in waits until you answer. The dashboard shows an **Approval needed** card on every page with **Allow**, **Allow view only** and **Deny**. The client sees an open connection but no pictures meanwhile; with no answer in 60 seconds it is turned away. Decisions go into the event log.

## The dashboard

![The Overview page of the Farpane Server dashboard](screenshots/server-overview.png){: width="1100" height="760" loading="lazy"}

- **Overview**: status, four live figures, a traffic chart (1 minute, 5 minutes or 1 hour), the shared screen, the offered sign-in methods, connected clients, a live preview and recent events.
- **Clients**: protection, encoding, bitrate, round trip, **View only** toggle and **Disconnect** per client.

![The Clients page in the Slate theme listing three connected clients](screenshots/server-clients.png){: width="1100" height="760" loading="lazy"}

- **Users**: the same rules as the command line.

![The Users page listing three accounts with roles and an Add a user form](screenshots/server-users.png){: width="1100" height="760" loading="lazy"}

- **Security**: listen address, TLS, RSA-AES and view-only, saved to `server.toml`; also forgets the TLS certificate or RSA key. Changes apply at the next start.

![The Security page in the Light theme with settings and key management](screenshots/server-security.png){: width="1100" height="760" loading="lazy"}

- **Displays** and **Event log** (export as JSON).

The top bar has a search box for clients and events and **Pause sharing**, which stops new pictures and ignores input while clients stay connected.

## Files

The config folder is `/etc/farpane` if it exists, else `~/.config/farpane` (set `--config-dir` or `FARPANE_CONFIG_DIR`). It holds `users.toml`, `server.toml`, `tls/`, `ra2_key.pem` and, for Wayland, the restore token. Secrets are written with mode 0600.

## As a service

`packaging/linux/farpane-server.service` runs `farpane-server --headless --desktop` for the logged-in user. Without a user account or password it listens on loopback only.

## Limits

One display, no Windows or macOS capture yet, and no WebSocket listener for browsers.
