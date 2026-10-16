---
title: Troubleshooting
description: Fixes for common Farpane problems - graphics start-up on XWayland, Wayland sharing, certificate warnings, locked accounts and keyboard capture.
order: 7
---

# Troubleshooting

## The window does not appear, or fails with `Surface::configure`

Under XWayland the default graphics backend can fail. Start the app with the Vulkan backend:

```sh
WGPU_BACKEND=vulkan farpane-viewer
```

## Sharing a Wayland desktop shows nothing

Use `--wayland` (or `--desktop` inside a Wayland session). Sharing an X11 display through XWayland captures only X11 windows, and reading the root window can fail. The desktop shows an approval dialog the first time; the server waits up to three minutes for it.

## The viewer warns that a certificate changed

The server presents a different certificate than the one you pinned. If you regenerated it on purpose (the dashboard's Security page can forget the certificate), compare the new fingerprint with the one printed by the server before accepting it. If you did not, treat it as a possible attack.

## "The account is locked"

Five wrong passwords in 15 minutes lock a name for 15 minutes, including the right password. Wait, or fix the cause and try again later. Unknown names are locked the same way.

## The server refuses to start on a network address

A non-loopback address needs credentials. Add a user (`farpane-server user add NAME`), set a shared password with `--password-file`, or pass `--allow-no-auth` if you accept the risk.

## System shortcuts still go to my computer

Enable **Capture keys** (or set capture to *always* in Settings). Capture works on X11 and Wayland; Windows and macOS are not supported yet. Press and release Ctrl+Alt+Shift to take the keyboard back. The Super (Windows) key cannot be forwarded.

## Characters turn into `?` in the clipboard

When either side lacks the Extended Clipboard extension, the legacy Latin-1 clipboard is used and other characters are replaced. With the extension, text travels as UTF-8.

## Something else

Open an [issue](https://github.com/pierrejochem/farpane/issues) with the version, platform, server you connect to and the event log.
