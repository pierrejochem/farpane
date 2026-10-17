# Contributing

Thanks for helping. The full conventions are in [`CLAUDE.md`](CLAUDE.md) (it doubles as the developer guide); the short version:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets   # must stay warning-free (pedantic is on)
cargo test --workspace
```

- Build prerequisites on Linux: Rust 1.88+, and for the Wayland backend `libpipewire-0.3-dev`, `pkg-config` and `clang`. Some tests start a private `Xvfb` or nested `Xwayland`.
- `rfb-proto` stays sans-IO and `no_std + alloc`. Protocol numbers come only from `rfb_proto::consts`, with a source.
- New wire messages need byte-exact golden tests, a round-trip test and a split-input test.
- `unsafe` is denied workspace-wide; platform code that needs it goes in its own crate with a written justification.
- Never put "VNC" in a product or crate name (it is RealVNC's trademark); say "VNC-compatible".
- Security problems: see [`SECURITY.md`](SECURITY.md), not a public issue.

By contributing you agree that your work is licensed under the project licence (MIT OR Apache-2.0).
