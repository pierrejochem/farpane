---
title: Install and build
description: How to build Farpane Viewer and Farpane Server from source on Linux, macOS and Windows, and what the release packages will contain.
order: 2
---

# Install and build

There is no release yet, so for now you build from source. Packages for Windows, macOS and Linux are produced by the release workflow when a version tag is pushed; they will be listed on the [GitHub releases page](https://github.com/pierrejochem/farpane/releases).

## Requirements

- Rust 1.85 or newer (`rustup` is the easiest way).
- On Linux, building the Wayland backend needs `libpipewire-0.3-dev`, `pkg-config` and `clang`. On Debian and Ubuntu:

```sh
sudo apt-get install -y libpipewire-0.3-dev pkg-config clang
```

## Build

```sh
git clone https://github.com/pierrejochem/farpane.git
cd farpane
cargo build --release --workspace
```

The binaries are `target/release/farpane-viewer` and `target/release/farpane-server`.

## Run

```sh
# the viewer window
./target/release/farpane-viewer

# connect straight to a server
./target/release/farpane-viewer myhost:5900

# a server that shares the test pattern on loopback, to try things out
./target/release/farpane-server --listen 127.0.0.1:5900
```

On a Wayland desktop that uses XWayland, the graphics layer may fail to start with its default backend. Setting `WGPU_BACKEND=vulkan` works around it (see [troubleshooting](troubleshooting.html)).

## Release packages

Pushing a tag `vX.Y.Z` builds, for each app:

| Platform | Package |
|---|---|
| Windows | MSI and zip |
| macOS | DMG |
| Debian, Ubuntu | `.deb` |
| Fedora, RHEL | `.rpm` |
| Arch, Manjaro | `.pkg.tar.zst` |
| Any glibc 2.34 or newer | AppImage and `.tar.gz` |

Each release carries `SHA256SUMS` and a GitHub build-provenance attestation, which you can check with `gh attestation verify <file> -R pierrejochem/farpane`. Releases are unsigned until signing accounts for Windows and macOS exist.
