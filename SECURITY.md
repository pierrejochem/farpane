# Security policy

Farpane is early software and has not been released. It has had one independent security review, but it has not been audited by a third party.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: [Report a vulnerability](https://github.com/pierrejochem/farpane/security/advisories/new). Do not open a public issue for them.

Include what you found, how to reproduce it, and the version or commit. You will get an answer as soon as possible; this is a one-person project, so please allow a few days.

## What is in scope

- The protocol core and decoders (`rfb-proto`, `rfb-codec`): malformed input that crashes, hangs or uses unbounded memory.
- Authentication and encryption (`rfb-security`, `rfb-tokio`, the server's accounts, lockout and TLS handling).
- The server and viewer applications.

## Supported versions

Only the latest commit on `main` until a first release exists.
