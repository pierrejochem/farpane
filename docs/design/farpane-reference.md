# Farpane – VNC/RFB Reference for a Rust Client & Server

Oct 15, 2026 · @Pierre Jochem

Purpose: one consolidated reference of the VNC wire protocol (RFB) – core, extensions, security, interop pitfalls and the current Rust ecosystem – as the basis for designing and building a Rust replacement for both client (viewer) and server.

## 1. Specifications and status

| Document | Status | Use it for |
| --- | --- | --- |
| RFC 6143 – The Remote Framebuffer Protocol (Richardson & Levine, March 2011) | IETF *Informational*, not Standards Track. Describes RFB 3.8 plus differences to 3.3/3.7. One verified erratum (EID 4951). | Normative baseline: handshake, core messages, Raw/CopyRect/RRE/Hextile/TRLE/ZRLE, Cursor + DesktopSize pseudo-encodings. |
| Community spec `rfbproto/rfbproto` (rfbproto.rst on GitHub, \~4,900 lines) | Living document, started 2009 by the TigerVNC devs; aims to document every extension in the wild. TigerVNC calls it the best reference. | Everything beyond 3.8: Tight, VeNCrypt, RA2, Fence, ContinuousUpdates, ExtendedDesktopSize, Extended Clipboard, QEMU/VMware extensions, H.264. |
| IANA "Remote Framebuffer (RFB)" registry | Official number allocations | Security types, message types, encoding numbers. New numbers must be allocated (via RealVNC/IANA) – never invent your own. |
| RealVNC RFB 3.8 PDF / older 3.3 & 3.7 PDFs | Historical | Only for legacy behaviour questions. |

Key rule from the spec: extend RFB only through new encodings, pseudo-encodings or security types – **never** change the protocol version string, otherwise you are no longer VNC compatible.

## 2. Protocol model in one page

- **Roles:** server = side whose framebuffer changes (desktop/VM); client = viewer with display + input.
- **Single drawing primitive:** "put a rectangle of pixel data at x,y". A *FramebufferUpdate* is a list of rectangles; each rectangle carries an *encoding type*.
- **Pull-based:** classic RFB is demand-driven – the server sends an update only after a *FramebufferUpdateRequest*. Slow clients/links automatically get fewer updates and skip transient states. ContinuousUpdates (+ Fence) switches to push mode.
- **Client is (nearly) stateless:** it can reconnect and see the same session; it must, however, keep a full copy of the framebuffer because the server sends incremental updates and may CopyRect from any region.
- **Byte order:** all integers big-endian, except pixel values (per negotiated pixel format) and a few extensions (gii) that use endian-flagged types.
- **Transport:** any reliable byte stream. Conventionally TCP 5900+N (display N), reverse/"listening viewer" on 5500, WebSocket for browser clients (noVNC), UNIX sockets locally.
- **Strings:** historically Latin-1/undefined; new implementations should send UTF-8 and must tolerate invalid UTF-8 without losing sync.

## 3. Connection phases

### 3.1 ProtocolVersion

Server sends 12 ASCII bytes `RFB 003.008\n`; client answers with the version it will use (≤ server's). Published versions: 3.3, 3.7, 3.8. Treat 3.5 as 3.3. Apple's server announces 3.889 – map unknown 3.x to the nearest supported version.

### 3.2 Security negotiation

- **3.7+:** server sends `U8 count` + `U8[count]` security types; client picks one byte. `count = 0` → failure, followed by `U32 len + reason`.
- **3.3:** server dictates a `U32` type (0 = fail, 1 = None, 2 = VNC Auth).
- Then type-specific handshake.

### 3.3 SecurityResult

`U32`: 0 OK, 1 failed, 2 too many attempts (Tight only). From 3.8 a failure is followed by a reason string. In 3.3/3.7 a None auth skips SecurityResult entirely (common bug source).

### 3.4 Initialisation

- **ClientInit:** `U8 shared-flag` (non-zero = leave other clients connected).
- **ServerInit:** `U16 width, U16 height, PIXEL_FORMAT (16 bytes), U32 name-len, name`. With Tight security active, followed by capability lists (server msgs, client msgs, encodings).

### 3.5 PIXEL\_FORMAT (16 bytes)

`bits-per-pixel (8/16/32), depth, big-endian-flag, true-colour-flag, red-max U16, green-max U16, blue-max U16, red-shift, green-shift, blue-shift, 3 pad`. Colour-map mode (true-colour = 0) uses SetColourMapEntries. Some servers report depth = bpp. The server must always be able to deliver the client's requested format → server needs a fast pixel converter.

## 4. Security types

| # | Name | Notes / recommendation |
| --- | --- | --- |
| 0 | Invalid | failure marker |
| 1 | None | only on loopback/inside tunnel |
| 2 | VNC Authentication | 16-byte challenge, DES-ECB with password (max 8 chars) as key, **bit-reversed key bytes**. Weak (8-char limit, DES, no encryption, offline crackable). Support for compatibility only. |
| 5 / 6 / 13 | RSA-AES (RA2), RA2ne (handshake-only), RA2r (two-step rekey) | RSA key exchange, AES-EAX framed transport (2-byte len as AAD, 16-byte LE counter nonce, 16-byte MAC), SHA-1 derived 128-bit keys. Username + password subtype. Supported by TigerVNC ≥ 1.13 and RealVNC-style clients. |
| 129 / 130 / 133 | RA2\_256 variants | Same, but SHA-256 keys/hashes → AES-256. Prefer over 5/6/13. |
| 16 | Tight | Tunnel + auth capability negotiation, extends ServerInit with capability lists; needed for TightVNC file transfer. |
| 18 | TLS (legacy, Vino) | anonymous TLS, then nested security list |
| 19 | VeNCrypt | Version 0.2 sub-negotiation. Subtypes: 256 Plain, 257 TLSNone, 258 TLSVnc, 259 TLSPlain, 260 X509None, 261 X509Vnc, 262 X509Plain, 263 TLSSASL, 264 X509SASL, 265 Ident, 266 TLSIdent, 267 X509Ident. **X509Plain / X509None (cert-validated TLS) is the de-facto strong option** across TigerVNC, QEMU, libvirt, wayvnc. "TLS\*" = anonymous TLS (MITM-able). |
| 20 | SASL | RFC 2222 mapping (QEMU/libvirt). Mechanisms negotiated; arbitrary number of steps; optional SASL-layer encryption. |
| 22 | xvp | VNC Auth + username/target (XenServer) |
| 30 | Diffie-Hellman (ARD, Apple) | DH + MD5 → AES-ECB credentials. Needed to talk to macOS Screen Sharing with user/password. |
| 113 | MSLogonII (UltraVNC) | 64-bit DH, trivially breakable |
| 3-4, 7-15, 128-255 (various) | RealVNC proprietary | not documented |
| 17 Ultra, 21 MD5, 23 Secure Tunnel, 24 Integrated SSH, 30-36 Apple Inc. (IANA; 30 = DH is documented) | registered, mostly undocumented |  |

**Recommended set for a new implementation:** server offers VeNCrypt (X509Plain, X509None, optional TLSVnc for legacy), RA2\_256, and VNC Auth only behind an explicit opt-in; None only on localhost/UNIX socket. Client supports all of: None, VncAuth, VeNCrypt (all TLS/X509 subtypes), RA2/RA2\_256 family, Apple DH (30) and, optionally, Tight (16) for TightVNC servers.

## 5. Client → Server messages

| Type | Name | Layout / notes |
| --- | --- | --- |
| 0 | SetPixelFormat | 3 pad + PIXEL\_FORMAT. Must not be sent with an update request outstanding (or use Fence SyncNext). Resets colour map. |
| 2 | SetEncodings | pad, `U16 n`, `S32[n]` – preference-ordered encodings + pseudo-encodings (= capability advertisement). |
| 3 | FramebufferUpdateRequest | `U8 incremental, U16 x,y,w,h`. Non-incremental = "I lost this area, resend fully, no CopyRect". Server crops to framebuffer. |
| 4 | KeyEvent | `U8 down, 2 pad, U32 keysym` (X11 keysyms). Autorepeat = repeated downs only. |
| 5 | PointerEvent | `U8 button-mask, U16 x, U16 y`. Bits: L, M, R, wheel up/down/left/right, back. ExtendedMouseButtons adds a 2nd byte when MSB set. |
| 6 | ClientCutText | 3 pad, `U32 len` (or negative `S32` with Extended Clipboard), Latin-1 text, LF line ends. |
| 150 | EnableContinuousUpdates | `U8 enable, U16 x,y,w,h` |
| 248 | ClientFence | 3 pad, `U32 flags`, `U8 len (≤64)`, payload. Flags: BlockBefore, BlockAfter, SyncNext, Request (bit 31). |
| 250 | xvp | shutdown / reboot / reset |
| 251 | SetDesktopSize | pad, `U16 w,h, U8 nscreens, pad, SCREEN[n]` (SCREEN = `U32 id, U16 x,y,w,h, U32 flags`) |
| 252 | Tight (file transfer etc.) | via Tight capabilities |
| 253 | gii | General Input Interface (extra axes/devices, endian-flagged) |
| 255 | QEMU | sub 0 = Extended Key Event (`U16 down, U32 keysym, U32 XT keycode`), sub 1 = Audio |
| 7–21, 80, 127–128, 249, 254 | UltraVNC / VMware / misc | mostly vendor-specific (UltraVNC file transfer, chat, scaling, keepalive…) |

All optional messages may only be sent after the peer confirmed support (usually via a pseudo-encoding rectangle in a FramebufferUpdate).

## 6. Server → Client messages

| Type | Name | Notes |
| --- | --- | --- |
| 0 | FramebufferUpdate | pad, `U16 n-rects`, then n × (`U16 x,y,w,h, S32 encoding` + data). n = 0xFFFF together with LastRect = "unknown count". |
| 1 | SetColourMapEntries | pad, `U16 first, U16 n`, n × (`U16 r,g,b`) |
| 2 | Bell | – |
| 3 | ServerCutText | as ClientCutText |
| 150 | EndOfContinuousUpdates | confirms CU support / end of CU |
| 248 | ServerFence | mirror of ClientFence |
| 250 | xvp | init/fail |
| 252 | Tight | file transfer replies |
| 253 | gii | version / device-creation response |
| 255 | QEMU | sub 1 = Audio stream |

## 7. Encodings (pixel data)

| # | Encoding | Description | Priority |
| --- | --- | --- | --- |
| 0 | Raw | width × height × bpp pixels. Mandatory. | MVP |
| 1 | CopyRect | `U16 src-x, src-y` – copy inside client framebuffer. Huge win for scrolling/window moves. | MVP |
| 2 | RRE | background + list of solid sub-rects | legacy (decode only) |
| 4 | CoRRE | RRE with U8 coordinates | legacy (decode only) |
| 5 | Hextile | 16×16 tiles, sub-encoding mask: Raw(1), BackgroundSpecified(2), ForegroundSpecified(4), AnySubrects(8), SubrectsColoured(16); bg/fg carry over between tiles | decode; encode optional |
| 6 | zlib | Raw through one persistent zlib stream | decode |
| 7 | **Tight** | Per-rect compression-control byte: low 4 bits reset zlib streams 0–3; high nibble = Fill (solid), JPEG, PNG (TightPNG) or basic compression with stream id + optional filter (Copy, Palette ≤256 colours, Gradient). TPIXEL = 3 bytes RGB when 32bpp/depth 24. Compact length (1–3 bytes). Data < 12 bytes sent uncompressed. **Most important for WAN/browsers.** | MVP (client), early (server) |
| 8 | zlibhex | Hextile + zlib | rare |
| 15 | TRLE | Tiled RLE (16×16), the uncompressed core of ZRLE | decode |
| 16 | **ZRLE** | One zlib stream per *connection* (never reset), data = `U32 len` + compressed; 64×64 tiles; CPIXEL (3 bytes for 24-bit TC); tile sub-encodings 0 raw, 1 solid, 2–16 packed palette, 128 plain RLE, 130–255 palette RLE. Lossless, best for LAN/text. | MVP |
| 17 | ZYWRLE | wavelet-lossy variant of ZRLE (LibVNC) | optional |
| 21 | JPEG | whole-rect JPEG (rare) | optional |
| 50 | Open H.264 | per-rect `U32 length, U32 flags` (reset context / reset all), Annex-B H.264 stream; supported by noVNC/LibVNC-based servers | phase 3 |
| -260 | TightPNG | Tight variant with PNG instead of zlib (noVNC) | optional |

RFC 6143 itself only defines Raw, CopyRect, RRE, Hextile, TRLE and ZRLE. IANA also registers 20 H.264 (RealVNC), 22 JRLE, 23 VA H.264, 24 ZRLE2 and Apple ranges 1000–1002, 1011, 1100–1109 – all undocumented; ignore them.

## 8. Pseudo-encodings (capability flags)

| # | Name | Purpose |
| --- | --- | --- |
| -23 … -32 | JPEG Quality Level | Tight JPEG quality 9…0 |
| -223 | DesktopSize | server-initiated resize (legacy semantics inconsistent) |
| -224 | LastRect | ends update early when n-rects = 0xFFFF |
| -232 | PointerPos | server pointer position |
| -239 | Cursor (RichCursor) | client-side cursor: pixels + 1-bit mask; hotspot in x/y – essential for latency |
| -240 | XCursor | 2-colour cursor |
| -247 … -256 | Compression Level | 9…0 |
| -257 | QEMU Pointer Motion Change | relative/absolute mouse switch |
| -258 | QEMU Extended Key Event | enables scancode key events (layout-independent!) |
| -259 | QEMU Audio | audio streaming |
| -261 | QEMU LED State | Caps/Num/Scroll lock sync |
| -305 | gii |  |
| -307 | DesktopName | rename session |
| -308 | **ExtendedDesktopSize** | multi-screen + client-requested resize. Rect: x = reason (0 server, 1 this client, 2 other client), y = status (0 OK, 1 prohibited, 2 out of resources, 3 invalid layout), w/h = new fb size; data `U8 nscreens, 3 pad, SCREEN[]`. Unlocks SetDesktopSize. |
| -309 | xvp |  |
| -312 | **Fence** | ordering/latency measurement |
| -313 | **ContinuousUpdates** | push mode |
| -314 | Cursor With Alpha | RGBA cursor (encoded with any real encoding) |
| -316 | ExtendedMouseButtons | back/forward buttons |
| -317 | Tight Without Zlib | Tight without zlib (e.g. when transport already compresses) |
| -412 … -512 | JPEG Fine-Grained Quality | 0–100 (TurboVNC) |
| -763 … -768 | JPEG Subsampling | 4:4:4 / 4:2:0 / gray … (TurboVNC) |
| 0x574D5664 … 0x574D566A | VMware cursor, cursor state/position, key repeat, LED, display mode, VM state |  |
| 0xC0A1E5CE | **Extended Clipboard** | negative length in Cut Text → U32 flags + payload. Formats: text, RTF, HTML, DIB, files (bits 0–4). Actions: caps (24), request (25), peek (26), notify (27), provide (28). Caps carry max size per format; provide payload is zlib-compressed. UTF-8, CRLF→LF rules. |

CRITICAL: a client must assume an extension is **unsupported** until it receives the server's confirmation (e.g. the first ExtendedDesktopSize rectangle, a ServerFence, an EndOfContinuousUpdates).

## 9. Behaviour that matters for a good implementation

- **Latency / flow control:** classic request/response caps throughput at one update per RTT. TigerVNC's approach: Fence + ContinuousUpdates, with the server estimating bandwidth/RTT via fences and limiting in-flight data ("congestion control"). Plan this in from the start for the server.
- **Persistent compression state:** zlib streams in zlib/ZRLE/Tight live for the whole connection – never drop a rectangle mid-stream; a decoder error is fatal. Model each stream as long-lived state in the decoder.
- **DesktopSize resizing:** after a size change the framebuffer content is undefined; client must reallocate and request a full update. Layout-only changes (ExtendedDesktopSize with same size) keep the content.
- **Keyboard:** keysyms are layout-dependent; shift is only a hint, servers may need to fake Shift/AltGr. The QEMU Extended Key Event (XT scancodes) avoids most layout problems – implement it in both client and server. Ignore lock keys unless LED-state extension is used. Viewer should send Tab+Shift instead of ISO\_Left\_Tab.
- **Clipboard:** legacy = Latin-1 only; Extended Clipboard = UTF-8 + rich formats + lazy transfer (notify/request).
- **Cursor:** render locally (Cursor/CursorWithAlpha) to hide latency; server must stop drawing the cursor into the framebuffer for clients that accept it.
- **Shared sessions:** server must broadcast resize/layout to all clients (reason 2), honour shared-flag, and keep per-client state (pixel format, encodings, pending region, zlib streams).
- **Robustness:** bound every length field (cut text, names, rect counts, rect area vs. framebuffer, zlib output size). Historically most VNC CVEs are memory-safety bugs in decoders/handshakes – Rust removes the class but not DoS via huge allocations.

## 10. Existing Rust ecosystem (status Oct 2026)

| Crate / project | Side | What it offers | Gaps |
| --- | --- | --- | --- |
| `rustvncserver` (Dustin McAfee, v2.x, Apache-2.0) | Server | Pure Rust, Tokio-based, RFB 3.8 / RFC 6143, 11 encodings via separate `rfb-encodings` crate, VNC Auth, reverse connections, CopyRect helper, TurboJPEG wrapper | No modern security (VeNCrypt/RA2) advertised; check Fence/CU/ExtendedDesktopSize support |
| `rfb-encodings` | Both | reusable encoder implementations | encoder-focused |
| `vnc-rs` (HsuJv, v0.5.x, MIT/Apache) | Client | async client engine, separated from UI, builds for native and WASM; Tight, ZRLE, CopyRect, Raw; VNC Auth | only tested against few servers, \~44% documented, no TLS/RA2 |
| `vncrs` | Server (Windows) | RFB 3.8, DES auth, FPS cap, session stats | Windows only, basic |
| `libvnc` / `libvnc-sys` | Both | FFI bindings to LibVNCServer/LibVNCClient | unsafe C dependency – contradicts the goal |
| whitequark `rust-vnc` | Client | early pure-Rust client/proxy | unmaintained |

Conclusion: no Rust crate covers the full modern feature set (VeNCrypt/RA2 + Fence/ContinuousUpdates + ExtendedDesktopSize + Extended Clipboard + QEMU keys). Use existing crates as reference/test targets; build own protocol core.

**Useful building blocks:** `tokio` + `tokio-util` (codec), `bytes`, `rustls` (VeNCrypt, cert validation), `flate2` (zlib with persistent `Compress`/`Decompress`, `FlushCompress::Sync`), `zune-jpeg` or `turbojpeg` (Tight JPEG), `png`, `des` (VNC auth – remember bit reversal), `rsa` + `aes` + `eax` + `sha1`/`sha2` (RA2), `num-bigint` (Apple DH), `openh264` (H.264), `xkeysym` (keysym tables), `tokio-tungstenite` (WebSocket for noVNC), `cargo-fuzz`/`arbitrary` (decoder fuzzing), `proptest`.

## 11. Proposed Rust architecture

**Workspace layout**

- `rfb-proto` – *sans-IO* core: message types, parse/serialize (`no_std` + `alloc` capable), state machines for handshake (client & server role), version 3.3/3.7/3.8 differences, strict length limits. No async, no sockets → fuzzable and reusable (WASM, embedded).
- `rfb-security` – trait `SecurityHandler` (negotiate → optional stream wrapper). Implementations: None, VncAuth, VeNCrypt(rustls), RA2/RA2\_256 (AES-EAX framing as a stream layer), AppleDH, SASL (feature-gated).
- `rfb-codec` – encoders/decoders: Raw, CopyRect, RRE/CoRRE/Hextile (decode), ZRLE/TRLE, Tight(+JPEG/PNG), zlib, H.264 (feature). Shared `PixelFormat` converter with SIMD fast paths (32bpp BGRX ↔ wire formats). Each decoder owns its persistent zlib state.
- `rfb-client` – async (Tokio) connection, event stream (`FramebufferUpdate`, `Resize`, `Cursor`, `Clipboard`, `Bell`), input API, auto-negotiation of pseudo-encodings, Fence/CU support. Framebuffer trait so UI can be `winit`+`wgpu`, `egui`, or WASM canvas.
- `rfb-server` – async server: per-client session task, shared `Framebuffer` with damage tracking (dirty-region set, tile hashing for change detection, CopyRect hints from the capture backend), encoder selection per client (bandwidth/RTT/CPU heuristics like TigerVNC's), congestion control via fences, broadcast of resizes.
- Backends (separate crates): screen capture (Linux: PipeWire/Wayland screencopy, X11 XShm/XDamage; macOS: ScreenCaptureKit; Windows: DXGI Desktop Duplication), input injection (uinput/libei, CGEvent, SendInput), virtual framebuffer (headless).
- Frontends: `rfb-viewer` (desktop), WebSocket proxy (`websockify` replacement) for noVNC.

**Design decisions to settle**

- Async runtime: decided, runtime-independent sans-IO core with a Tokio adapter first (section 18).
- Concurrency model on server: one task per client + shared framebuffer behind `Arc<RwLock>`/double buffer vs. actor that pushes damage to clients. Decided: hybrid, see section 23.
- Pixel pipeline: always store server FB as 32bpp native, convert per client lazily; cache encoded tiles only for stateless encodings, later (section 23).
- Security default: refuse unauthenticated network connections; force TLS/RA2 unless explicitly configured.
- Compatibility matrix as CI tests: TigerVNC (viewer + Xvnc), RealVNC Viewer, noVNC, QEMU built-in server, macOS Screen Sharing, UltraVNC, TightVNC, wayvnc, x11vnc, libvncserver.

## 12. Implementation roadmap (suggestion)

1. **MVP core:** RFB 3.3/3.7/3.8 handshake both roles, None + VncAuth, Raw + CopyRect + ZRLE, Cursor, DesktopSize, KeyEvent/PointerEvent, legacy clipboard. Interop with TigerVNC & QEMU.
2. **Security:** VeNCrypt (TLS/X509 × None/Vnc/Plain), RA2\_256 family, Apple DH (client). Fuzz all parsers.
3. **Performance:** Tight (+JPEG quality/compression pseudo-encodings), Hextile/zlib decoders, damage tracking + CopyRect detection in server, Fence + ContinuousUpdates + congestion control.
4. **Desktop features:** ExtendedDesktopSize/SetDesktopSize (multi-monitor, dynamic resize), CursorWithAlpha, Extended Clipboard, QEMU Extended Key Event + LED state, DesktopName, ExtendedMouseButtons.
5. **Extras:** H.264, WebSocket/noVNC, audio (QEMU), file transfer (Tight/UltraVNC – vendor-specific, only if needed), reverse connections.

## 13. Security considerations

- VNC Auth alone gives no confidentiality and an 8-character, offline-crackable secret – treat as legacy; never expose a VNC server with only VncAuth/None to an untrusted network.
- "TLS\*" VeNCrypt subtypes use anonymous TLS → man-in-the-middle possible; prefer X509\* with certificate validation (or pinning/TOFU in the client).
- RA2: the client must verify the server's RSA public key (fingerprint/TOFU) – otherwise MITM.
- Reverse-bit DES quirk, RA2 little-endian nonce and AEAD framing independent of RFB message boundaries are common implementation traps.
- Rate-limit auth attempts (Tight's "too many attempts" status), constant-time compares, zeroize secrets.
- Recent TigerVNC/Xvnc security advisories (e.g. July/Oct 2025) concern mostly the bundled X server code, not the RFB layer – a Rust server without X server code avoids that surface.

## 15. Protocol core plan (`rfb-proto`)

`rfb-proto` is a sans-IO, `no_std + alloc` crate that turns bytes into typed events and typed messages into bytes, for both roles. Sockets, TLS, crypto and pixel decoding stay outside, so the core is small, deterministic and fuzzable.

### Scope

| In the core | Outside the core |
| --- | --- |
| Wire types, constants, message parse/serialize | TCP/WebSocket/TLS transport (`rfb-client`, `rfb-server`) |
| Handshake state machines, client and server, versions 3.3/3.7/3.8 | DES, RSA, AES-EAX, SASL crypto (`rfb-security`, behind a trait) |
| Rectangle *framing*: finding where each rect payload ends | Pixel decoding/encoding, zlib, JPEG (`rfb-codec`) |
| Pseudo-encoding payloads (Cursor, ExtendedDesktopSize, DesktopName, LastRect, LED state) | Damage tracking, encoder choice, congestion control |
| Extension negotiation state (what is advertised vs. confirmed) | UI, capture, input injection |
| Hard limits on every length field | Configuration loading |

### Module layout

| Module | Contents |
| --- | --- |
| `consts` | Verified numbers: message types, security types, VeNCrypt subtypes, encodings, pseudo-encodings |
| `types` | `PixelFormat`, `Rect`, `Screen`, `EncodingType(i32)`, `SecurityType(u8)`, `KeySym(u32)`, `ButtonMask` |
| `wire` | `Reader` over `&[u8]` returning `NeedMore(n)` or `Malformed`; `Writer` into `Vec<u8>`/`BytesMut` |
| `msg::client` | `enum ClientMessage` (SetPixelFormat … QemuExtendedKey, SetDesktopSize, Fence, EnableContinuousUpdates) |
| `msg::server` | `enum ServerMessage` (UpdateBegin, SetColourMapEntries, Bell, CutText, Fence, EndOfContinuousUpdates) |
| `framing` | Per-encoding framers that measure a rect payload without decoding pixels |
| `pseudo` | Typed pseudo-rect payloads |
| `handshake` | `ClientHandshake`, `ServerHandshake`, `SecurityStep` trait, VeNCrypt/RA2 message layouts |
| `caps` | Advertised and confirmed extensions; `can_send()` guard |
| `clipboard` | Extended Clipboard flags, caps, notify/request/provide framing (zlib via trait) |
| `keys` | QEMU XT scancode helpers; keysyms via the `xkeysym` crate |

### Public API sketch

```rust
pub struct ClientConn { /* state, caps, pixel format, fb size, limits */ }

impl ClientConn {
    pub fn new(cfg: ClientConfig) -> Self;
    /// Consume bytes from `buf`; returns the next event or Ok(None) when more input is needed.
    pub fn receive(&mut self, buf: &mut BytesMut) -> Result<Option<ClientEvent>, ProtoError>;
    /// Serialize a message; refuses messages whose extension is not confirmed.
    pub fn send(&mut self, msg: &ClientMessage, out: &mut BytesMut) -> Result<(), ProtoError>;
    /// Security hand-off: core asks, `rfb-security` answers.
    pub fn security_reply(&mut self, step: SecurityOutput) -> Result<(), ProtoError>;
}

pub enum ClientEvent {
    SecurityOffered(Vec<SecurityType>),
    Security(SecurityInput),          // e.g. VncAuth challenge, VeNCrypt subtypes, StartTls
    ServerInit { width: u16, height: u16, pf: PixelFormat, name: Vec<u8> },
    UpdateBegin { rects: Option<u16> }, // None = unknown count (LastRect)
    Rect { hdr: RectHeader, payload: Bytes },
    Pseudo(PseudoRect),               // Cursor, ExtendedDesktopSize, DesktopName, ...
    UpdateEnd,
    ColourMap { first: u16, colours: Vec<[u16; 3]> },
    Bell, CutText(CutText), Fence(Fence), EndOfContinuousUpdates,
}
```

`ServerConn` mirrors this: `ServerEvent::{ClientInit, SetPixelFormat, SetEncodings, UpdateRequest, Key, Pointer, CutText, Fence, ...}` and `send_update(rects)` taking already-encoded payloads.

### Handshake state machine (client side)

1. `AwaitVersion` – read 12 bytes, pick min(server, 3.8); unknown 3.x maps to 3.3 or 3.8.
2. `AwaitSecurity` – 3.7+: list of types; 3.3: one `U32`; count 0 → read reason, fail.
3. `Security(Box<dyn SecurityStep>)` – steps exchange bytes; a step may return `StartTls` or `InstallCipher`, which the IO layer performs before resuming.
4. `AwaitSecurityResult` – skipped for None on 3.3/3.7; reason string only on 3.8.
5. Send ClientInit → `AwaitServerInit` (plus Tight capability lists when security 16 was used).
6. `Normal` – message loop; on `SetPixelFormat` the conn tracks the new format for framing.

The server machine is the mirror image. Version differences live in one `VersionPolicy` table, not in scattered `if`s.

### Rectangle framing – the key design decision

Rect payloads are not self-delimiting: ZRLE has a `U32` length, Raw is w × h × bytes-per-pixel, Tight uses compact lengths per part, Hextile and RRE must be walked. Plan: the core ships a *framer* per encoding that scans just enough bytes to find the payload end, then emits `Rect { payload: Bytes }`. Decoding then runs in `rfb-codec`, possibly on another thread, while the core keeps parsing.

| Encoding | Framing cost |
| --- | --- |
| Raw, CopyRect, Cursor | fixed size from header + pixel format |
| ZRLE, zlib, H.264 | length prefix |
| RRE, CoRRE | count × fixed sub-rect size |
| Tight | control byte + compact length(s); palette/filter headers sized by pixel format |
| Hextile | walk 16×16 tiles, read sub-encoding mask and sub-rect counts |
| Unknown encoding | fatal: stream cannot be resynchronised |

Trade-off: framing Hextile and Tight duplicates a little parsing that the decoder also does. The gain is a clean split between protocol and pixels and zero-copy hand-off.

### Constants (verified against IANA + community spec)

```rust
pub mod enc {
    pub const RAW: i32 = 0;            pub const COPY_RECT: i32 = 1;
    pub const RRE: i32 = 2;            pub const CO_RRE: i32 = 4;
    pub const HEXTILE: i32 = 5;        pub const ZLIB: i32 = 6;
    pub const TIGHT: i32 = 7;          pub const ZLIB_HEX: i32 = 8;
    pub const TRLE: i32 = 15;          pub const ZRLE: i32 = 16;
    pub const ZYWRLE: i32 = 17;        pub const JPEG: i32 = 21;
    pub const OPEN_H264: i32 = 50;     pub const TIGHT_PNG: i32 = -260;
}
pub mod pseudo {
    pub const JPEG_QUALITY_0: i32 = -32;      pub const JPEG_QUALITY_9: i32 = -23;
    pub const DESKTOP_SIZE: i32 = -223;       pub const LAST_RECT: i32 = -224;
    pub const CURSOR: i32 = -239;             pub const X_CURSOR: i32 = -240;
    pub const COMPRESS_0: i32 = -256;         pub const COMPRESS_9: i32 = -247;
    pub const QEMU_POINTER_MOTION: i32 = -257;
    pub const QEMU_EXT_KEY: i32 = -258;       pub const QEMU_AUDIO: i32 = -259;
    pub const QEMU_LED: i32 = -261;           pub const GII: i32 = -305;
    pub const DESKTOP_NAME: i32 = -307;       pub const EXT_DESKTOP_SIZE: i32 = -308;
    pub const XVP: i32 = -309;                pub const CLIENT_REDIRECT: i32 = -311;
    pub const FENCE: i32 = -312;              pub const CONTINUOUS_UPDATES: i32 = -313;
    pub const CURSOR_WITH_ALPHA: i32 = -314;  pub const COLOR_MAP: i32 = -315;
    pub const EXT_MOUSE_BUTTONS: i32 = -316;  pub const TIGHT_NO_ZLIB: i32 = -317;
    pub const FINE_QUALITY_0: i32 = -512;     pub const FINE_QUALITY_100: i32 = -412;
    pub const SUBSAMP_FIRST: i32 = -768;      pub const SUBSAMP_LAST: i32 = -763;
    pub const VMWARE_CURSOR: i32 = 0x574D_5664;
    pub const EXT_CLIPBOARD: i32 = 0xC0A1_E5CE_u32 as i32;
}
```

All encoding values are sent as `S32` on the wire. The VMware values stay positive; Extended Clipboard (0xC0A1E5CE) exceeds i32::MAX, so it becomes negative and needs the u32 cast.

### Limits (defaults, all configurable)

| Field | Default limit | Why |
| --- | --- | --- |
| Framebuffer width/height | 16,384 px each | caps allocation from a hostile ServerInit/resize |
| Rect | must lie inside the framebuffer (pseudo-rects exempt) | prevents out-of-bounds writes in decoders |
| Rects per update | 65,535, or unbounded only with LastRect | spec maximum |
| SetEncodings entries | 1,024 | real clients send < 50 |
| Desktop name, reason strings | 64 KiB |  |
| Legacy cut text | 1 MiB |  |
| Extended Clipboard, after inflate | 16 MiB | zip-bomb guard |
| Fence payload | 64 bytes | spec maximum |
| Screens in ExtendedDesktopSize | 255 | spec maximum |
| Handshake duration | 30 s (enforced by IO layer) | slow-loris guard |

Every error except "need more bytes" is fatal and closes the connection: RFB has no resync point.

### Error model

`ProtoError::{Malformed { ctx }, LimitExceeded { field, value }, UnexpectedMessage(u8), UnsupportedVersion, UnsupportedEncoding(i32), ExtensionNotConfirmed(&'static str), SecurityFailed(Option<String>)}`. `ctx` names the field, so fuzz crashes and field reports point at the exact byte.

### Test plan

- **Golden vectors:** byte-exact fixtures for every message table in the community spec, both directions.
- **Round-trip property tests** (`proptest`): `parse(serialize(m)) == m` for all message types and pseudo payloads.
- **Fuzzing** (`cargo-fuzz`): targets for client-incoming, server-incoming, each framer, Extended Clipboard; run in CI for a short time per PR, longer nightly.
- **Trace replay:** recorded sessions against TigerVNC, QEMU, noVNC/websockify, macOS Screen Sharing, x11vnc, replayed through the core with expected event sequences.
- **Split-input tests:** feed every fixture byte by byte to prove `NeedMore` handling is correct.

### Milestones

1. **M1 – skeleton:** `consts`, `types`, `wire`, core six client and four server messages, 3.8 handshake with None and VncAuth hook. Golden vectors.
2. **M2 – legacy + framing:** 3.3/3.7 handshake paths for client and server, framers for Raw, CopyRect, RRE, CoRRE, Hextile, zlib, ZRLE, TRLE; Cursor, DesktopSize, LastRect pseudo-rects. Fuzz targets live.
3. **M3 – modern extensions:** Tight/TightPNG framer, ExtendedDesktopSize + SetDesktopSize, Fence, ContinuousUpdates, Extended Clipboard, QEMU Extended Key + LED, DesktopName, CursorWithAlpha, ExtendedMouseButtons.
4. **M4 – security hooks:** VeNCrypt negotiation with `StartTls`, RA2/RA2\_256 framing with `InstallCipher`, Apple DH and Tight security message layouts; trace replay against real servers.

### Decisions (2026-10-15)

- **`bytes` in the core:** yes. `Rect` payloads are handed out as `Bytes` slices of the receive buffer (zero-copy); input is `BytesMut`. `bytes` builds without `std`, so `no_std + alloc` stays.
- **Rect scanning in the core:** yes. Framers for every supported encoding live in `rfb-proto::framing`; `rfb-codec` only ever sees complete payloads.
- **RFB 3.3 support:** yes, in both roles. The client accepts 3.3 servers; the server offers 3.8 and accepts clients that answer 3.3 or 3.7 (and 3.5 treated as 3.3), restricted to security types None and VncAuth as 3.3 allows.

## 16. Dashboard UI (pure Rust)

Both apps get a dashboard-style GUI built with **egui/eframe on wgpu**. The same stack draws the remote framebuffer as a GPU texture and the live charts, so viewer and server share one UI toolkit, one theme and one widget library.

### Framework choice

| Option | Fit for these apps | Verdict |
| --- | --- | --- |
| **egui + eframe (wgpu)** | Framebuffer = wgpu texture with partial uploads per dirty rect; `egui_plot` for live charts; custom widgets are cheap; AccessKit accessibility; runs on Windows, macOS, Linux (X11 + Wayland) | **Chosen** |
| Slint | Declarative `.slint` markup, polished look out of the box; custom GPU texture possible but more work; license is GPLv3, royalty-free (with attribution) or commercial | Strong alternative if visual polish beats flexibility |
| iced | Elm-style architecture, wgpu backend; fewer ready dashboard widgets | Viable, more own widget work |
| Tauri / Dioxus desktop | UI in a webview (HTML/CSS) | Rejected: not a pure-Rust UI, extra webview dependency |

Immediate-mode egui looks plain by default, so "fancy" comes from a shared `farpane-ui` crate: custom dark/light theme, card and stat-tile widgets, sparklines, status badges, icon font and smooth animations.

### Themes (user's choice)

The user picks a theme in Settings › Appearance or from the quick switch in each top bar; the choice applies to viewer and server and is stored in the settings TOML. The remote framebuffer is never recoloured.

| Theme | Surfaces | Accent | Typeface | Rendering cost |
| --- | --- | --- | --- | --- |
| Glass (default) | Frosted panels over a dark shattered-slab backdrop, sheen highlight, deep shadow, 18 px radius | Ember #E8492F / #F4623A | Inter | Needs a blur pass behind panels |
| Slate | Solid dark panels, flat, denser, 14 px radius | Cyan #38BDF8 | IBM Plex Sans | Minimal |
| Light | White cards on #F3F4F6, soft shadows, 16 px radius | Ember darkened to #B5361F for text contrast | Inter | Minimal |
| System | Light when the OS is in light mode, Glass in dark mode; switches live | Follows | Follows | Varies |

Implementation in `farpane-ui`:

- One `Theme` struct of semantic tokens (bg, text, muted, faint, line, inset, selection, card fill, card shadow, radius, accent, accent text, button fill/foreground, status green/blue/amber/red/violet, chart series, font family, blur on/off); widgets read only tokens, never raw colours.
- Three built-in `Theme` constants, mapped to egui `Visuals` plus custom painters for cards, badges, switches and charts.
- `ThemePreference { System, Glass, Slate, Light }` persisted in settings; System listens to the OS appearance change (winit `ThemeChanged`).
- Glass blur: render the backdrop once to a texture, keep a pre-blurred copy (downsample + Gaussian, recomputed on resize) and draw each panel's region of it under a translucent fill. That gives frosted glass without a per-frame blur.
- Status colours have separate light-theme values so every badge keeps 4.5:1 contrast.
- Mockup reference: Farpane Dashboards canvas (Viewer, Session, Server, Appearance).

Further appearance options, all stored next to the theme preference:

| Option | Values | Effect |
| --- | --- | --- |
| Accent colour | Theme default, Ember, Cyan, Violet, Green, Amber | Replaces the accent token (buttons, active items, switches, first chart series). Light theme uses darker variants so text keeps 4.5:1 contrast. |
| Reduce transparency | On / off | Glass panels become solid and the blur pass is skipped; also saves GPU time. Follows the OS setting by default. |
| Interface size | 90 %, 100 %, 115 % | Scales all UI (egui `pixels_per_point`); the remote screen keeps its own scaling. |
| Reduce motion | On / off | Turns off panel and toolbar animations. Follows the OS setting by default. |

### Viewer dashboard

- **Home:** address book as cards (name, host, last seen, security badge, thumbnail of the last session), quick-connect bar accepting `host:display` and `vnc://` URLs.
- **Session window:** framebuffer canvas (fit / 1:1 / scale), collapsible stats overlay: FPS, round-trip time from Fence, bandwidth, active encoding, JPEG quality; sparkline of the last 60 s.
- **Connection details panel:** negotiated version, security type, TLS/RA2 key fingerprint (trust-on-first-use prompt), pixel format, enabled extensions.
- **Controls:** send Ctrl+Alt+Del, clipboard sync on/off, view-only, resize-to-window (ExtendedDesktopSize), monitor picker for multi-screen servers.

### Viewer fullscreen

Fullscreen shows only the remote desktop; all controls move into a hidden toolbar at the top edge.

| Aspect | Behaviour |
| --- | --- |
| Enter / exit | Toolbar button, F11 (Windows, Linux), Ctrl+Cmd+F (macOS), double-click on the session title. Exit with the same keys or the toolbar's exit button. |
| Toolbar | Slim tab at top centre; hover at the top edge or click the tab to slide down a pill with: host and resolution, keyboard-capture status, display choice, Ctrl+Alt+Del, capture switch, stats toggle, pin, exit, Disconnect. Auto-hides after 2 s unless pinned. |
| Keyboard capture (macOS permission asked at first launch) | On by default in fullscreen: system shortcuts (Alt+Tab, Win key, Cmd+Tab, Cmd+Space) go to the remote machine. Release chord Ctrl+Alt+Shift (configurable) returns them; a hint shows the chord on entry for 6 s. |
| Resolution | With "Resize remote" enabled the viewer sends SetDesktopSize for the monitor's native size, so the image is 1:1. Otherwise it scales to fit. |
| Displays | "This display" (default) or "Span all": one window per local monitor, each mapped to a remote screen from ExtendedDesktopSize. |
| Stats | Compact one-line overlay in the top-right corner, toggled from the toolbar or a hotkey. |

Implementation notes (winit + egui):

- `Window::set_fullscreen(Some(Fullscreen::Borderless(monitor)))`; borderless keeps fast switching and works on Wayland, X11, Windows and macOS.
- Keyboard grab: Windows low-level keyboard hook while focused; macOS `CGEventTap` (needs Accessibility permission, prompt on first use); Wayland `zwp_keyboard_shortcuts_inhibit_manager_v1`; X11 `XGrabKeyboard`. Always released on focus loss.
- Span all: one borderless window per monitor sharing the decoded framebuffer texture, each showing its screen's rectangle.
- The toolbar is drawn by egui over the framebuffer texture; the framebuffer stays at native resolution when no scaling is needed.
- Mockup: "Viewer · Fullscreen" artboard, reached from the session toolbar.

### Server dashboard

- **Overview tiles:** running state, listening address and port, connected clients, total bandwidth, encode CPU time.
- **Live preview** of the shared screen(s) with the regions currently being sent highlighted.
- **Clients table:** address, user, security type, encoding, bitrate, RTT, view-only toggle, disconnect button; per-client sparkline.
- **Charts:** bandwidth and FPS over time, encoding mix, update latency.
- **Security settings:** enabled security types (VeNCrypt X509, RA2\_256, VncAuth opt-in), certificate management, passwords, allowed networks; incoming-connection approval prompts.
- **Event log:** connects, auth failures, resizes, errors; export as JSON.
- **Headless mode:** `--headless` runs the same core without a window (systemd user unit, Windows service later).

### Architecture

- Each app = thin binary over libraries: `farpane-viewer`/`farpane-server` (Tokio, background threads) + `farpane-ui` (shared widgets/theme).
- UI ↔ core via channels: core sends `UiEvent` (stats snapshots \~4 per second, frame-dirty notifications, prompts); UI sends `Command` (connect, disconnect, settings). The UI thread never blocks on network I/O.
- Framebuffer path (viewer): decoder writes into a CPU framebuffer; the UI uploads only dirty rects with `queue.write_texture` once per frame, so a 4K desktop with small changes costs little bandwidth to the GPU.
- Stats come from one `Metrics` struct per connection (counters + ring buffers), shared by the dashboard, the log and a future Prometheus endpoint.
- Settings persisted as TOML in the platform config dir; secrets in the OS keychain (`keyring` crate).

## 17. Distribution via GitHub Releases

A tag `vX.Y.Z` triggers `.github/workflows/release.yml`, which builds both apps on GitHub-hosted runners and attaches all packages to a draft release for review. Pre-release tags (`-rc.N`) become pre-releases. Workflow and packaging files: `farpane-release-packaging.zip` (shared in chat), linted with actionlint + shellcheck; the Linux configs were test-built with nfpm 2.43.0.

| Platform | Architectures | Package formats | Signing |
| --- | --- | --- | --- |
| Windows 10/11 | x64, ARM64 (cross-built on x64) | `.msi` (WiX 5: both apps, Start-menu shortcuts, `vnc://` handler, firewall rule), portable `.zip` | Azure Artifact Signing via OIDC (exe + msi) |
| macOS 12+ | Universal (arm64 + x86\_64 via `lipo`) | one `.dmg` per app (`.app` bundle) | Developer ID, hardened runtime, notarized + stapled |
| Debian 12+, Ubuntu 22.04+ | amd64, arm64 | `.deb` per app | SHA256SUMS + provenance |
| Fedora, RHEL/Alma/Rocky 9+, openSUSE | x86\_64, aarch64 | `.rpm` per app | SHA256SUMS + provenance |
| Arch and derivatives | x86\_64, aarch64 | `.pkg.tar.zst` per app | SHA256SUMS + provenance |
| Any distro with glibc ≥ 2.34 | x86\_64, aarch64 | `.AppImage` per app, portable `.tar.gz` | SHA256SUMS + provenance |

- Linux builds run in an AlmaLinux 9 container, so one binary covers all listed distros (glibc floor 2.34).
- Native Linux packages are made with nfpm from one YAML per app; the server ships a systemd *user* unit for headless mode, because screen capture needs the user's session.
- Signing steps are skipped when their secrets are missing, so forks and early builds still release unsigned.
- Every file gets a GitHub build-provenance attestation and a line in `SHA256SUMS`.
- Later: Homebrew cask, winget, AUR `-bin`, Flathub, hosted apt/dnf repos, in-app update check.

Open before the first release: final product and binary names, maintainer and license fields, bundle IDs, real icons, Apple Developer and Azure signing accounts.

## 18. Decision log (2026-10-16)

The open questions from the review are answered below; only the product name and two release items remain open.

| # | Question | Decision |
| --- | --- | --- |
| 1 | Product name | **Farpane** (decided 2026-10-16). Apps "Farpane Viewer" and "Farpane Server", binaries `farpane-viewer` and `farpane-server`, shared UI crate `farpane-ui`; protocol crates keep the generic `rfb-*` names. "RVNC" was dropped: RealVNC holds "VNC" as a registered trademark (EU, US; US reg. 3641372 covers remote-access software), and `rvnc` was already taken on crates.io. TMview found no conflicting Farpane mark in classes 9/42; `farpane` is free on crates.io. Describe the product as "VNC-compatible", never with VNC in the name. |
| 2 | Unattended access | **Yes.** Server runs as a system service, works at the login screen and after reboot (section 19). |
| 3 | Sign-in accounts | **Server's own user list**, independent of OS accounts (section 20). |
| 4 | Platforms for the first server release | **All four:** Linux Wayland, Linux X11, macOS, Windows. |
| 5 | File transfer | **Own extension**, with numbers registered at IANA (section 21). |
| 6 | Browser access | **In v1:** built-in WebSocket listener, noVNC-compatible (section 21). |
| 7 | H.264 | **Encoding 50 (Open H.264), constrained-baseline stream from the OS hardware encoder, OpenH264 as fallback;** off by default, Tight/ZRLE stay the baseline (section 22). |
| 8 | Audio and rich clipboard formats | **Confirmed for later phases.** |
| 9 | Runtime | **Runtime-independent core** (`rfb-proto` sans-IO); Tokio adapter first, blocking and WASM adapters possible. |
| 10 | Server concurrency | **Hybrid:** one capture/damage task broadcasting change notices, one task per client owning its encoder state (section 23). |
| 11 | Encoded-tile caching | **Later, and only for stateless encodings** (section 23). |
| 12 | Appearance | **More options:** accent colour, reduce transparency, interface size, reduce motion (section 16, mockup "Settings · Appearance"). |
| 13 | Keyboard capture | **Ask for the macOS Accessibility permission at first launch**, with a reminder when entering fullscreen if it was declined; release keys Ctrl+Alt+Shift. |
| 14 | Signing accounts | **Later.** Builds stay unsigned until Apple Developer and Azure Artifact Signing accounts exist. |

**Still open**

- Secure the name: file an EU trademark for "Farpane" in classes 9 and 42 (ideally after a lawyer's clearance search; this is not legal advice), publish placeholder crates `farpane`, `farpane-viewer`, `farpane-server`, `farpane-ui`, and register the GitHub organisation and a domain.
- Minimum OS versions (macOS 12, glibc 2.34) are still assumptions.
- The release workflow and the interop test hosts have not run yet.

## 19. Unattended access

The server splits into a privileged **system service** (network listener, authentication, user list, policy, audit log) and an unprivileged **session worker** per graphical session (capture, input injection, clipboard). The service starts at boot, follows log-in, log-out and user switches, and restarts the worker in whichever session is on screen, including the login screen where the platform allows it.

| Platform | Service | Worker in the user's session | Login screen | Notes |
| --- | --- | --- | --- | --- |
| Windows 10/11 | Windows service as LocalSystem; reacts to session-change notifications | Started by the service in the active console session with a SYSTEM token (`CreateProcessAsUser`); DXGI Desktop Duplication, `SendInput` | Yes: the SYSTEM worker switches to the secure Winlogon desktop (also covers UAC prompts) | Ctrl+Alt+Del via `SendSAS`; the installer enables software SAS generation. RDP sessions are not shared, only the console. |
| macOS 12.3+ | LaunchDaemon (root): network, auth, policy | LaunchAgent per user session: ScreenCaptureKit, `CGEventPost` | Pre-login agent (Apple's PreLoginAgents pattern), as Apple's DTS recommends; capture before login is not guaranteed on macOS 15, must be tested early | Needs Screen Recording and Accessibility permissions. Request Apple's `com.apple.developer.persistent-content-capture` entitlement for screen-sharing products, otherwise macOS 15 asks users to re-approve recording regularly. MDM profiles can pre-approve Accessibility; Screen Recording can only be delegated to the user. |
| Linux X11 | systemd system unit (root) | Worker per X display as the session user: XShm + XDamage + XTest | Yes, attaching to the display manager's X greeter (LightDM, SDDM on X11) | GDM normally uses a Wayland greeter; see below. |
| Linux Wayland, physical screen | systemd system unit | Worker in the user's session via xdg-desktop-portal ScreenCast + RemoteDesktop, saving the portal's restore token after the first approval | No generic way. wlroots compositors (sway, labwc) allow capture and input through their own protocols, as wayvnc does. | Unattended after one approval while the user is logged in. Treat the restore token as an opaque string (TigerVNC's w0vncserver had a bug assuming a UUID). |
| Linux, virtual sessions | systemd system unit | Server starts its own headless desktop per user (headless Wayland compositor or virtual X server), like TigerVNC's `vncsession` | Not needed: the server shows its own login, authenticates against its user list, then starts or resumes that user's virtual desktop | Most reliable unattended mode for Linux servers and machines without a monitor. |

Security rules for unattended mode:

- Only encrypted security types are offered: VeNCrypt X.509 or RA2-256. VNC password and anonymous TLS are refused unless the admin enables them explicitly.
- Per-user policy decides whether a logged-in local user must approve the connection, gets a notice only, or is not asked.
- A visible on-screen indicator shows while anyone is connected, and every connection and failed sign-in goes to the audit log.
- Packaging changes needed: add a systemd **system** unit, a Windows service registration in the MSI, and a LaunchDaemon plus LaunchAgent plists in the macOS package. The current user unit stays for the non-admin case.

## 20. Server user accounts

The server keeps its own user list, separate from OS accounts. Passwords are stored as Argon2id hashes, which works for every security type that sends a username and password inside an encrypted channel.

| Security type | Credentials on the wire | Checked against |
| --- | --- | --- |
| VeNCrypt X509Plain / TLSPlain | Username + password inside TLS | User list, Argon2id hash |
| RA2-256 / RA2 (subtype 1) | Username + password inside AES-EAX | User list, Argon2id hash |
| RA2 subtype 2 (password only) | Password only | Refused by default; allowed only in single-user mode |
| VNC password, X509Vnc, TLSVnc | DES challenge-response, no username | Needs the password itself (no hash possible): a separate legacy password, max 8 characters, stored encrypted with a machine key and mapped to one chosen account. Off by default. |

- **Storage:** `users.toml` in the system config folder (`/etc/farpane/`, `%ProgramData%\Farpane\`, `/Library/Application Support/Farpane/`), readable only by root/SYSTEM. Fields: name, display name, password hash, role, approval policy, allowed networks, enabled flag, created/last-login timestamps.
- **Roles:** Admin (all access, manages users and settings), Control (view and control), View only. Roles map to the existing view-only switch in the clients table.
- **Approval policy per user:** always ask the local user, ask only when someone is logged in, or never ask (unattended).
- **Lockout:** 5 failed sign-ins per user in 15 minutes lock that user for 15 minutes; failures per IP address add increasing delays. All of it lands in the event log.
- **Management:** a Users page in the server dashboard and a CLI (`farpane-server user add | passwd | disable | remove | list`). The first-run wizard creates the first admin; there are no default passwords.
- **Later:** one-time codes (TOTP) need a security type that can carry a second factor; RA2 and VeNCrypt Plain only carry username and password.

## 21. Own file transfer and browser access

### File transfer extension

File transfer is a new protocol extension of our own, so it needs official numbers before it ships: one pseudo-encoding (capability), one client-to-server and one server-to-client message type. The IANA RFB registry assigns them by expert review (the expert is from RealVNC), and the community spec requires that allocation before an extension is documented. **Apply early;** until numbers are assigned the feature stays behind a build flag and is only enabled between our own viewer and server.

- **Negotiation:** the viewer lists the pseudo-encoding in SetEncodings; the server confirms with a pseudo-rectangle carrying its limits (max chunk size, allowed operations, root folder name). Nothing is sent before that confirmation.
- **Framing:** each message is `type, subtype, U32 transfer-id, U32 length, payload`. Files move in chunks of at most 64 KiB so framebuffer updates and input are never blocked; the server schedules chunks behind screen updates.
- **Operations:** list folder, stat, download, upload, create folder, cancel, error. Uploads and downloads resume from an offset and end with a SHA-256 check.
- **Flow control:** a per-transfer window (acknowledged bytes), plus the existing Fence-based round-trip estimate.
- **Permissions:** Admin and Control roles may transfer, View only may not. The server restricts paths to a configured root folder (default: a Transfers folder in the user's home), normalises paths and refuses symlinks that point outside the root.
- **UI:** a transfer panel in the viewer session with drag-and-drop; a later step can attach files to the Extended Clipboard "files" format for copy-paste.

### Browser access (v1)

The server includes a WebSocket listener so browsers can connect with noVNC, without websockify.

- **Transport:** RFB inside WebSocket binary frames (RFC 6455) on a separate port (5800 in the mockups, configurable). It accepts connections with no subprotocol or the `binary` subprotocol; current noVNC sends none by default.
- **Encryption:** browsers connect over `wss://` with the same certificate as VeNCrypt. Plain `ws://` is accepted only from localhost.
- **Sign-in:** noVNC supports VeNCrypt only with the Plain subtype (no TLS inside VeNCrypt) and RA2ne (security type 6), not RA2 or RA2-256. On the WebSocket listener the server therefore offers VeNCrypt-Plain and RA2ne, which are safe because `wss` already encrypts everything, and checks them against the user list.
- **Origin check:** the server compares the `Origin` header with an allowlist to block cross-site connections from random web pages.
- **Optional web client:** the server can serve a bundled copy of noVNC (MPL-2.0, licence notice included) from the same port, so a browser only needs the server's address.

## 22. H.264

For compatibility the encoder library does not matter; the wire format and the H.264 profile do. Use **Open H.264 (encoding 50)** with a **Constrained Baseline** stream, which every H.264 decoder can play. A server only sends an encoding the viewer listed, so viewers without H.264 automatically get Tight or ZRLE instead.

| Viewer | Decodes encoding 50 |
| --- | --- |
| Our viewer | Yes |
| TigerVNC viewer | Since 1.13, optional (depends on how it was built) |
| noVNC (browser) | Since 1.6 |
| RealVNC Viewer | Not documented to support it; IANA lists separate, undocumented H.264 encodings (20, 23) registered by RealVNC. Not a target. |
| Other viewers | Fall back to Tight/ZRLE |

Encoder plan (server):

1. **OS hardware encoders first:** Media Foundation (Windows), VideoToolbox (macOS), VA-API (Linux Intel/AMD), NVENC optional. Low CPU, and on Windows and macOS the encoder ships with the OS. Some Linux distributions disable H.264 in their VA-API drivers, so Linux needs the fallback more often.
2. **OpenH264 as fallback:** Cisco pays the MPEG LA royalties only for its own prebuilt binary, which must be downloaded from Cisco when the product is installed or first used, not bundled in our packages. A build from source would make us responsible for the fees.
3. **Policy:** off by default for the first release. When the viewer supports it, the server switches large, fast-changing regions (video, 3D) to H.264 and keeps Tight/ZRLE for text and static content. Users can force it on or off per connection.

Viewer side: decode with the OS decoders (Media Foundation, VideoToolbox, VA-API) and use OpenH264 as fallback, under the same download rule.

## 23. Server concurrency and tile caching

**Concurrency: hybrid model.** It works the same for one viewer or fifty, and a slow viewer never holds back a fast one.

- **One capture task** per screen owns the framebuffer. After each capture it writes the new frame into a double buffer, bumps a frame number and publishes the changed region (plus CopyRect hints from the capture backend) on a broadcast channel.
- **One task per client** owns everything that is per connection: pixel format, encoding list, zlib/Tight streams, its own pending dirty region and its send budget from the Fence round-trip estimate. It merges change notices into its dirty region and encodes only when it is allowed to send, so a slow client simply skips intermediate frames.
- **Input and clipboard** go from client tasks to one input task, which serialises them before injection, so two viewers cannot interleave half key presses.
- This keeps the sans-IO core unchanged: each client task drives one `ServerConn`, and the runtime adapter decides whether tasks are Tokio tasks or threads.

**Encoded-tile caching: later, and only where it is safe.** ZRLE, zlib and Tight's zlib modes carry compression state per connection, so their output cannot be shared between clients. Raw, Hextile, Tight Fill and Tight JPEG/PNG are stateless and can be shared.

- Phase 3 (performance): cache stateless results keyed by (frame number, rectangle, pixel format, encoding, quality), evicted when the next frame lands. It pays off with several viewers on the same screen (classrooms, browser dashboards).
- H.264 streams are per client and never shared.
- Design the encoder API now so a stateless encoder can return reusable bytes, so the cache can be added later without changing callers.

## 14. Sources

- RFC 6143 – https://www.rfc-editor.org/rfc/rfc6143 (errata-inline: https://www.rfc-editor.org/rfc/inline-errata/rfc6143.html)
- RFB community specification – https://github.com/rfbproto/rfbproto/blob/master/rfbproto.rst
- IANA RFB registry – http://www.iana.org/assignments/rfb/rfb.xml
- Wikipedia: RFB (protocol) – https://en.wikipedia.org/wiki/RFB\_(protocol)
- TigerVNC developer wiki (DesktopSize survey, latency notes) – https://github.com/TigerVNC/tigervnc/wiki/Development
- Announcement of the community spec (2009) – https://lwn.net/Articles/331788/
- rustvncserver – https://docs.rs/rustvncserver
- vnc-rs – https://github.com/HsuJv/vnc-rs
- vncrs – https://docs.rs/crate/vncrs/0.1.6
- libvnc (Rust bindings) – https://docs.rs/crate/libvnc/latest
- TigerVNC security update (X server CVEs, 2025) – https://errata.msvsphere-os.ru/definition/9/INFSA-2025:9306?lang=en

Added 2026-10-16 (decisions):

- [RealVNC legal page (VNC, RealVNC, RFB trademarks)](https://www.realvnc.com/en/legal/)
- [US trademark VNC, reg. 3641372](https://trademarks.justia.com/771/98/vnc-77198888.html)
- [crates.io: rvnc](https://crates.io/crates/rvnc)
- [TigerVNC issue 1187: H.264 (encoding 50)](https://github.com/TigerVNC/tigervnc/issues/1187)
- [BLFS ticket 17616: TigerVNC 1.13 viewer H.264](https://wiki.linuxfromscratch.org/blfs/ticket/17616)
- [noVNC 1.6.0 release notes](https://newreleases.io/project/github/novnc/noVNC/release/v1.6.0)
- [noVNC rfb.js (supported security types)](https://github.com/novnc/noVNC/blob/master/core/rfb.js)
- [OpenH264 FAQ (licensing)](https://www.openh264.org/faq.html)
- [Apple Developer Forums: pre-login capture on macOS 15](https://developer.apple.com/forums/thread/768146)
- [Jump Desktop: macOS Sequoia screen recording policies](https://support.jumpdesktop.com/hc/en-us/articles/29070118000781-macOS-Sequoia-Screen-Recording-Policies-and-Jump-Desktop-Connect)
- [tigervnc-users: w0vncserver portal restore token](https://groups.google.com/g/tigervnc-users/c/cdv-YPjQvkU)

Verification (2026-10-15): every pseudo-encoding number in section 8 matches the IANA RFB registry (updated 2024-08-14) and noVNC's encodings.js. IANA lists two values the doc lacked: -311 ClientRedirect and -315 ColorMap. -257, -259 and -261 sit in the QEMU block -257 to -272; -232 sits in the Tight options block. IANA labels 0xC0A1E5CE–CF "possibly used in UltraVNC" – the community spec documents it as Extended Clipboard, which TigerVNC and noVNC use.
