# Farpane brand guidelines

Source of truth for how Farpane looks, sounds and what it may claim. The icon
masters stay in `docs/brand/`; the UI tokens stay in `crates/farpane-ui`. This
file only describes them.

## 1. What Farpane is

Farpane is a VNC-compatible remote desktop **viewer** and **server**, open
source, written in Rust. It speaks the standard RFB protocol, so it works with
the servers and viewers people already run, and adds the things they lack:
encryption that is on, accounts with roles, a dashboard, and a Wayland
backend.

**One line:** *Far screens, close at hand.*

**Descriptor (use under the name):** *VNC-compatible remote desktop viewer and
server. Open source, written in Rust.*

### Name

- Written **Farpane**, capital F, one word. Products: **Farpane Viewer**,
  **Farpane Server**.
- The name comes from the mark: panes that recede into the distance, and the
  one far pane you are looking at.
- **Never put "VNC" in a product, crate, package or domain name.** It is a
  RealVNC trademark. In running text say "VNC-compatible".
- Keep the disclaimer where the brand is shown at length (README, site footer,
  press kit): *"VNC" is a trademark of RealVNC Ltd. Farpane is an independent
  project and not affiliated with RealVNC.*

### Audience

1. **Self-hosters and admins** who run x11vnc, TigerVNC or QEMU consoles and
   want encryption, accounts and a working Wayland story without a cloud broker.
2. **Developers and infrastructure people** who care that the protocol core is
   small, sans-IO Rust that they can read, test and embed.
3. **Small IT teams** that need unattended access to Linux and Windows desktops
   from their own network.

Farpane is not for people who want a hosted service, a relay or a remote
support product with a customer-facing link. Say so; it is a strength.

## 2. Positioning

> For people who run their own remote desktops, Farpane is the VNC-compatible
> viewer and server that keeps the connection yours: open protocol, encrypted
> sign-in, no account with anyone else.

### Pillars

| Pillar | Message | Proof you may use |
|---|---|---|
| **Open** | Any RFB viewer or server works with it. MIT or Apache-2.0. | Viewer verified against TigerVNC 1.15, QEMU, macOS Screen Sharing, a kiosk VeNCrypt server; server against our own viewer and noVNC 1.5 in a browser. |
| **Protected** | Sign-in is encrypted and checked. | VeNCrypt over TLS (X.509, pinned fingerprints), RSA-AES; accounts hashed with Argon2id; lockout after repeated failures; optional approval of each client. |
| **Modern** | It runs where desktops are now. | Linux X11 and Wayland (through the desktop portal); Windows service with lock-screen capture; browser access through a WebSocket port; optional H.264. |
| **Plain** | You can see what it does. | Rust, a sans-IO protocol core, a handbook that lists what is verified and what is not. |

## 3. Voice

Calm, exact, a little dry. Written for someone who will read the config file.

- **Say what it does, then how you know.** "Connects to TigerVNC 1.15" beats
  "works with everything".
- **Say what it does not do.** Unbuilt things go in the same breath as built
  ones. Honesty is the brand's most useful feature.
- **Short sentences, active verbs, one idea each.** No exclamation marks.
- **Numbers and names over adjectives.** "Argon2id", not "military-grade".
- **Second person for instructions, third person for facts.** Avoid "we".
- **No fear, no hype.** Never "unhackable", "blazing", "revolutionary",
  "next-generation", "seamless", "effortless".

| Instead of | Write |
|---|---|
| Secure remote access | Encrypted sign-in (TLS or RSA-AES) |
| Works with all VNC software | Connects to any RFB server; verified against TigerVNC, QEMU and macOS Screen Sharing |
| Blazing fast | ZRLE, Tight and optional H.264; stats overlay shows the real numbers |
| Cross-platform | Builds and passes its tests on Linux, Windows and macOS; the server shares Linux (X11, Wayland) and Windows desktops |

## 4. Claims policy

Marketing may repeat only what the handbook (`docs/handbook/`) and
`CLAUDE.md` mark as verified. Before publishing, check each claim against them.

- **Status line:** while unreleased, say so: *"Early development, not yet
  released."* Drop it only when the first tagged release is out.
- **Not built, do not imply:** macOS server (capture and input), SASL,
  Wayland clipboard, hardware H.264 encoders, several displays, multi-monitor
  viewer layouts.
- **Not verified, do not claim:** compatibility with RealVNC products, macOS
  Screen Sharing clipboard and cursor, RSA-AES against a third-party server.
- **Never claim** certification, audits, or "secure" without the mechanism.
- Name third-party products only as compatibility facts, never as partners.

## 5. Visual identity

### The mark

"Receding panes" (concept 03). Three rounded squares step away toward the
upper right, growing fainter, to one small solid pane: the far pane.

- Masters: `docs/brand/farpane-mark.svg` (gradient), `farpane-mark-mono.svg`,
  `farpane-mark-light-tile.svg`, small-size variants `*-small.svg` (use below
  48 px), app icons `farpane-viewer.svg` and `farpane-server.svg`.
- Lockups: `farpane-logo-dark.svg` (for dark backgrounds),
  `farpane-logo-light.svg` (for light ones).
- **Clear space:** at least the height of the small pane on every side.
- **Minimum size:** 16 px for the mark, 96 px wide for the lockup.
- **Do not** recolour the gradient, rotate, outline, add shadows to the mark
  itself, or stretch it. Use the mono mark for one-colour printing.

### Colour

Colours come from the UI tokens, so product and marketing match.

| Role | Dark | Light |
|---|---|---|
| Background | `#07080A` | `#FAFAFA` |
| Card | `#101216` | `#FFFFFF` |
| Text | `#F1F1F3` | `#14171C` |
| Muted text | `#A3A8B2` | `#525B68` |
| Line | `#23262C` | `#E2E5EA` |
| Accent | `#E8492F` | `#C93C25` |
| Accent text | `#FF8A73` | `#B5361F` |

Brand gradient (the mark only): `#F4623A` to `#D23B21`. Far-pane tint:
`#FFB59C`. Accent text on light backgrounds uses `#B5361F` to keep contrast
above 4.5:1; on dark, `#FF8A73`.

Status colours (green, amber, red) belong to product states, not to marketing.

### Type

- **Inter** SemiBold for headlines, Regular for text. Used by the Glass and
  Light themes.
- **IBM Plex Sans** is the Slate theme's face; use it when showing Slate.
- **Monospace** for commands and protocol names (system `ui-monospace`).
- Both families are SIL OFL and ship in `crates/farpane-ui/assets/fonts/`.
- Headlines in sentence case, tight tracking (-0.02em), never all capitals.

### Imagery

- **Product first.** Show the real apps from `site/static/screenshots/`. Do
  not mock up screens that do not exist. Retake them after UI changes.
- **Panes as the motif.** Large, cropped, receding rounded rectangles in the
  mark's gradient at low opacity. No stock photos, no padlock clip-art, no
  hooded figures, no globe-and-network graphics.
- Dark backgrounds by default (the Glass theme); a light variant exists for
  print and documents.

## 6. Where things live

| What | Where |
|---|---|
| This file, copy, press kit | `docs/brand/marketing/` |
| Image sources (HTML) and renderer | `docs/brand/marketing/src/`, `render.py` |
| Rendered images | `docs/brand/marketing/assets/` |
| Social preview used by the site | `site/static/og.png` |
| Logo and icon masters | `docs/brand/` |
| Screenshots | `site/static/screenshots/` (`site/screenshots/make.sh`) |

Regenerate the images: `python3 docs/brand/marketing/render.py` (needs Google
Chrome or Chromium).
