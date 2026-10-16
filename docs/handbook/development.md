---
title: Development
description: How to build, test and contribute to Farpane - commands, conventions, continuous integration and the release process.
order: 9
---

# Development

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets   # must stay warning-free (pedantic is on)
cargo fmt --all
cargo doc --workspace --no-deps          # must build without warnings
```

## Conventions

- Protocol numbers come only from `rfb_proto::consts`, each with a source.
- New wire messages get byte-exact golden tests, a round-trip test and a split-input test (fed byte by byte).
- `unsafe` is denied across the workspace; platform code that needs it lives in its own crate with a written justification.
- Strings on the wire are UTF-8; invalid UTF-8 is accepted without losing sync.
- Widgets read design tokens, never raw colours.

## Tests that need a display

Some tests start a private `Xvfb`, or a nested `Xwayland` under a Wayland session, so your desktop is never touched. The viewer's QEMU tests skip themselves when `qemu-system-x86_64` is not installed. Fuzz targets for the protocol core live in `crates/rfb-proto/fuzz` and need nightly Rust and `cargo-fuzz`.

## Continuous integration

`ci.yml` checks formatting, clippy, tests (Ubuntu, macOS and Windows), the documentation build, and runs each fuzz target for 30 seconds. `release.yml` builds packages on a `vX.Y.Z` tag and attaches them to a draft release.

## Contributing

See `CONTRIBUTING.md` and `SECURITY.md` in the repository. The source is at [github.com/pierrejochem/farpane](https://github.com/pierrejochem/farpane).
