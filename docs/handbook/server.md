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
| `--desktop` | The desktop of the session the server runs in: on Windows the whole desktop (all monitors as one picture); elsewhere Wayland if `WAYLAND_DISPLAY` is set, else X11. |

On Windows, `--desktop` captures with DXGI Desktop Duplication (all monitors as one picture; the changed areas come from Windows) and falls back to GDI when duplication is refused (a driver without support, a rotated or HDR monitor, a desktop switch); it tries again every 5 seconds. Set `FARPANE_CAPTURE=gdi` to force GDI. Input uses `SendInput`; text clipboard works both ways. Run by hand as an ordinary user it cannot capture or control the secure desktop (UAC prompts, lock screen), while the service's worker can follow it (see below). Ctrl+Alt+Del cannot be sent.

Under the rootless XWayland of a Wayland desktop, capturing the X11 root fails, so use `--wayland` there.

## Who may connect

By default the server listens on `127.0.0.1:5900` and refuses a non-loopback address unless there are credentials (or you pass `--allow-no-auth`, which is a bad idea without a tunnel).

- **Accounts** (recommended): `farpane-server user add NAME` creates a user. The first user is an administrator. Roles are `admin`, `control` and `view` (view looks only). Passwords come from `FARPANE_PASSWORD`, `--password-file` or standard input, never from arguments, and are 8 to 255 bytes. Other commands: `passwd`, `role`, `disable`, `enable`, `remove`, `list`. The last enabled admin cannot be removed without `--force`.
- **Shared password**: `--password-file FILE` for VNC Auth when there are no accounts.

Accounts sign in by name and password inside encryption: TLS (`--tls-self-signed` or `--tls-cert` and `--tls-key`) or RSA-AES (`--ra2`). The server prints the certificate and RSA key fingerprints at start; give them to your users so they can compare them in the viewer.

## Browsers (noVNC)

`--ws ADDR` (or **WebSocket for browsers** on the Security page) adds a second port that speaks WebSocket, so a page running [noVNC](https://novnc.com/) can connect to `wss://HOST:PORT`. With TLS configured (`--tls-self-signed` or your own certificate) it is `wss`: the browser's TLS protects the connection and users sign in with user name and password (VeNCrypt Plain or RSA-AES without extra encryption, both safe inside `wss`). Without TLS the port is `ws` and is only accepted on this computer (a loopback address), because a browser would otherwise send passwords in the clear. A self-signed certificate makes the browser ask you to accept it first.

Only pages from the same host may use the port unless you list others with `--ws-origin https://pages.example` (repeatable; `*` allows any page, which makes the server's own sign-in the only protection). The server does not serve noVNC itself: host its files anywhere and point it at the port.

## H.264 video

H.264 is off by default. It needs Cisco's OpenH264 library, which is not part of Farpane: run `farpane-server h264 install` (or use **Download from Cisco** on the Security page) once; the file is fetched from Cisco, checked against the hash Cisco published and kept in the config folder. Then start with `--h264` or tick **Offer H.264 video**. Clients that ask for it first get the whole screen as one H.264 stream (Constrained Baseline); other clients are served as before.

## Approving connections

With `--require-approval` (or **Ask before a client gets in** on the Security page) every client that signs in waits until you answer. The dashboard shows an **Approval needed** card on every page with **Allow**, **Allow view only** and **Deny**. The client sees an open connection but no pictures meanwhile; with no answer in 60 seconds it is turned away. Decisions go into the event log.

## The dashboard

![The Overview page of the Farpane Server dashboard](screenshots/server-overview.png){: width="1100" height="760" loading="lazy"}

- **Overview**: status, four live figures, a traffic chart (1 minute, 5 minutes or 1 hour), the shared screen, the offered sign-in methods, connected clients, a live preview and recent events.
- **Clients**: protection, encoding, bitrate, round trip, **View only** toggle and **Disconnect** per client.

![The Clients page in the Slate theme listing three connected clients](screenshots/server-clients.png){: width="1100" height="760" loading="lazy"}

- **Users**: the same rules as the command line.

![The Users page listing three accounts with roles and an Add a user form](screenshots/server-users.png){: width="1100" height="760" loading="lazy"}

- **Security**: listen address, TLS, RSA-AES and view-only, saved to `server.toml` and applied to the running server at once (a command-line option still wins; clients already connected keep their connection); also forgets the TLS certificate or RSA key, which a new one replaces at the next start.

![The Security page in the Light theme with settings and key management](screenshots/server-security.png){: width="1100" height="760" loading="lazy"}

- **Displays** and **Event log** (export as JSON).

The top bar has a search box for clients and events and **Pause sharing**, which stops new pictures and ignores input while clients stay connected.

## Files

The config folder is `/etc/farpane` if it exists, else `~/.config/farpane` (set `--config-dir` or `FARPANE_CONFIG_DIR`). It holds `users.toml`, `server.toml`, `tls/`, `ra2_key.pem` and, for Wayland, the restore token. Secrets are written with mode 0600.

## As a service

`packaging/linux/farpane-server.service` runs `farpane-server --headless --desktop` for the logged-in user. Without a user account or password it listens on loopback only.

On Windows, `farpane-server service install [--auto] [OPTIONS]` registers a `FarpaneServer` service (the MSI does too, start on demand), `service uninstall` removes it and `sc start FarpaneServer` starts it. It keeps its files in `%ProgramData%\Farpane`, readable by administrators and SYSTEM only, and writes `service.log` there. The service itself has no screen (it runs in session 0), so it starts a session worker, `farpane-server service worker`, in the session on the physical console, with the service's SYSTEM rights. The worker captures the screen and injects input; the service keeps the network, accounts and policy. The two talk over anonymous pipes that only they hold. If the worker ends it is started again, and when the console moves to another user (fast user switching) it follows. Remote Desktop sessions are not shared, only the console. Because the worker is SYSTEM it also follows the input desktop to the sign-in screen, lock screen and UAC prompts; the installer and service were checked on Windows 11 with the user's desktop and with a worker restart, and the lock screen was captured through the service; UAC prompts have not been tried. Ctrl+Alt+Del cannot be sent.

## Limits

One display (on Windows all monitors form one picture), and no macOS capture yet.
