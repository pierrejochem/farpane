---
title: Security model
description: How Farpane protects remote desktop connections - TLS and RSA-AES, certificate pinning, Argon2id accounts, lockout and throttling, and what is not supported.
order: 5
---

# Security model

Farpane has had an independent review of its code but no third-party audit. Treat it as early software.

## Encryption

- **VeNCrypt over TLS** using X.509 certificates, through rustls. Only the certificate-validated subtypes are chosen. Anonymous TLS is refused, because it cannot be told from a man in the middle.
- **RSA-AES** (RA2, RA2_256, and the `ne` variants) with a pinned server key. It was cross-checked against an independent implementation, but not against a third-party server.
- **VNC Auth** is the legacy shared password. It authenticates but does not encrypt, and its DES challenge is weak; use it only inside a tunnel.

The viewer refuses to fall back to an unencrypted type for a server you have already pinned, so someone on the network path cannot talk it into plain VNC Auth.

## Trust

A server certificate or RSA key is trusted when its SHA-256 fingerprint matches a pin you accepted, or when a public or supplied CA signed the certificate for the host name. There is no operating-system trust store and no revocation checking.

## Accounts

The server keeps its own user list in `users.toml` (mode 0600). Passwords are hashed with Argon2id. A wrong user costs the same time as a wrong password, so timing does not show which names exist.

- **Lockout**: 5 failures in 15 minutes lock a name for 15 minutes; failures are counted before the slow check, so parallel guesses cannot get past it.
- **Throttle**: delays of 1, 2, 4 ... 30 seconds per address (an IPv6 /64 counts as one address) after failures.
- **Limits**: at most 4 password hashes at once, 32 connections per address and 256 in total by default.

## Browsers

The WebSocket port accepts user names and passwords only inside TLS (`wss`), and refuses `ws` on a network address. It checks the `Origin` of every browser connection so another web page cannot use a visitor's browser to reach the server. Frames are bounded and malformed ones end the connection.

## Approval

The server can hold every new client until you allow it (`--require-approval`), as view only if you like. An unanswered request is refused after 60 seconds.

## Input and clipboard

A view-only user or client has all keyboard, pointer and clipboard input ignored. Keys and buttons a client still holds are released when it disconnects or is made view-only. The viewer drops control characters from clipboard text it receives, so a remote machine cannot slip escape sequences into a terminal paste.

## Not supported

SASL, Apple DH, MS-Logon II, the two-step RSA-AES types, client certificates and allowed-network lists.

## Reporting a problem

Please use GitHub's [private vulnerability reporting](https://github.com/pierrejochem/farpane/security/advisories/new) rather than a public issue. See also `SECURITY.md` in the repository.
