# Release packaging

Pushing a tag `vX.Y.Z` (or `vX.Y.Z-rc.N`) runs `.github/workflows/release.yml`,
which builds both GUI apps and attaches everything to a **draft** GitHub Release.
Review the draft, then publish it.

## What a release contains

| Platform | Architectures | Files |
|---|---|---|
| Windows 10/11 | x64, ARM64 | `farpane-<ver>-windows-<arch>.msi` (both apps, Start-menu shortcuts, `vnc://` handler, firewall rule), portable `.zip` |
| macOS 12+ | Universal (Apple Silicon + Intel) | `farpane-viewer-<ver>-macos-universal.dmg`, `farpane-server-<ver>-macos-universal.dmg` |
| Debian 12+, Ubuntu 22.04+ | amd64, arm64 | `farpane-viewer_<ver>-1_<arch>.deb`, `farpane-server_<ver>-1_<arch>.deb` |
| Fedora, RHEL/Alma/Rocky 9+, openSUSE | x86_64, aarch64 | `.rpm` per app |
| Arch, Manjaro, CachyOS | x86_64, aarch64 | `.pkg.tar.zst` per app (`pacman -U`) |
| Any glibc ≥ 2.34 distro | x86_64, aarch64 | `.AppImage` per app, portable `.tar.gz` |
| All | – | `SHA256SUMS`, GitHub build-provenance attestation (`gh attestation verify <file> -R pierrejochem/farpane`) |

Linux binaries are built in an AlmaLinux 9 container, so the glibc floor is 2.34.

## Before the first release

1. Names are final (Farpane; binaries `farpane-viewer`/`farpane-server`; display names in
   `packaging/linux/*.desktop`, the single source for the macOS app names). Maintainer, homepage and
   licence in `packaging/linux/nfpm-*.yaml`, the bundle IDs `io.github.pierrejochem.farpane.*` in
   `packaging/macos/*.Info.plist` and the Manufacturer in `packaging/windows/farpane.wxs` are set;
   change the bundle IDs only before the first release, since macOS ties app data and permissions to them.
2. Icons are final (concept 03, "Receding panes"): `icons/svg/` holds the masters (full and
   small versions, plus a tile-less, one-colour and light-tile mark), `icons/png/` has 16–1024 px,
   and `.ico` / `.icns` files are ready. Sizes of 48 px and below use the simplified small version.
   The Windows `.exe` files carry their icon and version information: `apps/*/build.rs` embeds
   `icons/farpane-*.ico` with the `winresource` crate (needs `rc.exe`, which the Windows runner
   has; a failure is a build warning, not an error).
3. Keep the `UpgradeCode` GUID in `farpane.wxs` forever once v1 ships.
4. `farpane-server.service` runs `--headless --desktop` (no window; share the session's desktop).
   Without a user account or password the server listens on loopback only: add users with
   `farpane-server user add NAME` and enable `--tls-self-signed` / `--ra2` (edit `ExecStart`)
   before exposing it. On Wayland the desktop asks for approval the first time.

## Windows service

`farpane.wxs` registers a `FarpaneServer` service (LocalSystem, start on demand, removed on
uninstall). `farpane-server service install [--auto] [OPTIONS]` does the same without the MSI,
`service uninstall` removes it, and Windows starts it with `farpane-server service run`. Its files
live in `%ProgramData%\Farpane` (locked to administrators and SYSTEM at install time) and it logs
to `service.log` there. A service runs in session 0 and cannot capture the screen, so until the
session worker of section 19 and a Windows capture backend exist, it serves the test pattern only.
The service code (`crates/farpane-winsvc`) has been type-checked for Windows, not run on it.

## Signing (optional — steps are skipped when secrets are missing)

Unsigned builds work, but users see Gatekeeper / SmartScreen warnings.

### macOS (Developer ID, Apple Developer Program)

| Secret | Content |
|---|---|
| `APPLE_CERT_P12` | base64 of the *Developer ID Application* certificate + key (`.p12`) |
| `APPLE_CERT_PASSWORD` | password of that `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_NOTARY_KEY_P8` | base64 of an App Store Connect API key (`.p8`) |
| `APPLE_NOTARY_KEY_ID` | its key ID |
| `APPLE_NOTARY_ISSUER_ID` | its issuer ID |

Apps are signed with the hardened runtime, the DMGs are notarized and stapled.
The server asks for *Screen Recording* and *Accessibility* permission on first run.

### Windows (Azure Artifact Signing)

New OV/EV certificates must live in hardware, so a `.pfx` secret is usually not
possible any more. The workflow uses Microsoft's Azure Artifact Signing with OIDC
(no stored password).

| Name | Kind | Content |
|---|---|---|
| `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` | secrets | App registration with a federated credential for this repo; role *Artifact Signing Certificate Profile Signer* |
| `AZURE_SIGNING_ENDPOINT` | variable | e.g. `https://weu.codesigning.azure.net/` |
| `AZURE_SIGNING_ACCOUNT` | variable | signing account name |
| `AZURE_SIGNING_PROFILE` | variable | certificate profile name |

Both `.exe` files and the `.msi` are signed and timestamped.

## Local test of the Linux packages

```sh
cargo build --release
VERSION=0.0.0-dev NFPM_ARCH=amd64 nfpm package -f packaging/linux/nfpm-farpane-server.yaml -p deb -t dist/
```

## Later (not in this workflow yet)

- Homebrew tap (`brew install --cask`), winget manifest, AUR `-bin` package, Flathub.
- Hosted apt/dnf repositories so updates arrive through the system updater.
- In-app update check against the GitHub Releases API.
