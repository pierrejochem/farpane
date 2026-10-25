use core::fmt;
use std::{collections::VecDeque, time::Instant};

use rfb_codec::{
    Cursor, Damage, DecodeError, Decoder, DesktopLayout, Framebuffer,
    clipboard::{self, ExtClipboard, Role},
    enc,
};
use rfb_proto::{
    PixelFormat,
    conn::ClientConnEvent,
    consts::{fence, pseudo},
    msg::client::ClientMessage,
    types::{Rect, Screen},
};
use rfb_security::tls::Fingerprint;
use rfb_tokio::{ClientOptions, ClientSession, Error, ServerInfo, TlsOptions};
use tokio::net::TcpStream;

use crate::stats::{Collector, Snapshot};

/// Encodings we ask for, most preferred first. ZRLE usually compresses best;
/// Raw is always understood by servers.
pub const DEFAULT_ENCODINGS: [i32; 13] = [
    enc::ZRLE,
    // No JPEG quality level is requested, so a server sends only the
    // lossless Tight methods.
    enc::TIGHT,
    enc::HEXTILE,
    // Uncompressed tiles: cheaper to decode than ZRLE, bigger on the wire.
    enc::TRLE,
    enc::ZLIB,
    enc::CO_RRE,
    enc::COPY_RECT,
    enc::RAW,
    pseudo::CURSOR,
    // Both size encodings: a server must send only the extended one to a
    // client that asks for both, and old servers still understand the other.
    pseudo::EXT_DESKTOP_SIZE,
    pseudo::DESKTOP_SIZE,
    // UTF-8 clipboard; a server that lacks it keeps using the legacy messages.
    pseudo::EXT_CLIPBOARD,
    // Round-trip measurement; a server that lacks it just never answers.
    pseudo::FENCE,
];

/// What to authenticate with and which TLS certificates to trust.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// For VNC Auth, and for `VeNCrypt` `X509Plain` together with `username`.
    pub password: Option<Vec<u8>>,
    pub username: Option<Vec<u8>>,
    /// Certificate fingerprints the user trusts for this server.
    pub pins: Vec<Fingerprint>,
    /// DER certificates of extra CAs to trust.
    pub extra_roots: Vec<Vec<u8>>,
    /// Ask for H.264 and decode it with this codec. `None` (the default) never
    /// asks for it.
    pub video: Option<std::sync::Arc<dyn rfb_codec::video::VideoCodec>>,
    /// zlib level 0 to 9 to ask the server for (lower is faster to encode and
    /// bigger on the wire). `None` leaves it to the server.
    pub compression: Option<u8>,
    /// Picture quality 0 to 9 for lossy encodings. `None` leaves it to the
    /// server.
    pub quality: Option<u8>,
    /// Ask the server to push updates without a request each (it needs
    /// Fence too). Off by default.
    pub continuous_updates: bool,
}

/// The host part of `host:port`, `[v6]:port` or a bare host: the name a
/// server's certificate has to match.
#[must_use]
pub fn host_of(addr: &str) -> &str {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match addr.matches(':').count() {
        1 => addr.split(':').next().unwrap_or(addr),
        _ => addr, // no port, or a bare IPv6 address
    }
}

/// Why connecting or running failed.
#[derive(Debug)]
pub enum ConnectError {
    Session(Error),
    Decode(DecodeError),
    Io(std::io::Error),
    /// The server sent a damaged Extended Clipboard message.
    Clipboard(clipboard::ClipboardError),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(e) => write!(f, "{e}"),
            Self::Decode(e) => write!(f, "decode error: {e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Clipboard(e) => write!(f, "clipboard: {e}"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<Error> for ConnectError {
    fn from(e: Error) -> Self {
        Self::Session(e)
    }
}

impl From<DecodeError> for ConnectError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

/// A connected remote screen: the session, its decoder and the picture.
#[derive(Debug)]
pub struct RemoteScreen {
    session: ClientSession<TcpStream>,
    decoder: Decoder,
    fb: Framebuffer,
    name: String,
    stats: Collector,
    /// The screens from the last accepted layout, to build resize requests.
    screens: Vec<Screen>,
    clip: ExtClipboard,
    /// Fence requests sent and not yet answered: sequence number and send time.
    pings: VecDeque<(u64, Instant)>,
    next_ping: u64,
    /// We asked for `ContinuousUpdates`, so the server may announce it.
    wants_continuous: bool,
    /// Continuous updates are on for a picture of this size: the server sends
    /// updates by itself, and per-update requests would only add traffic.
    continuous: Option<(u16, u16)>,
}

impl RemoteScreen {
    /// Connect to `addr` (`host:port`), authenticate, switch the server to our
    /// native pixel format and ask for a full first update.
    ///
    /// # Errors
    /// Connection, handshake (including a wrong or missing password), or
    /// setup errors.
    pub async fn connect(addr: &str, password: Option<Vec<u8>>) -> Result<Self, ConnectError> {
        Self::connect_with(addr, password, &DEFAULT_ENCODINGS).await
    }

    /// Like [`RemoteScreen::connect`], asking for `encodings` (most preferred
    /// first) instead of the default list. Handy to test one encoding at a time.
    ///
    /// # Errors
    /// As [`RemoteScreen::connect`].
    pub async fn connect_with(
        addr: &str,
        password: Option<Vec<u8>>,
        encodings: &[i32],
    ) -> Result<Self, ConnectError> {
        let options = ConnectOptions {
            password,
            ..ConnectOptions::default()
        };
        Self::connect_opts(addr, &options, encodings).await
    }

    /// The general form: credentials and trusted certificates in `options`.
    /// An untrusted TLS certificate is reported as
    /// `ConnectError::Session(Error::Untrusted { .. })` with its fingerprint.
    ///
    /// # Errors
    /// As [`RemoteScreen::connect`].
    pub async fn connect_opts(
        addr: &str,
        options: &ConnectOptions,
        encodings: &[i32],
    ) -> Result<Self, ConnectError> {
        let stream = TcpStream::connect(addr).await.map_err(ConnectError::Io)?;
        stream.set_nodelay(true).map_err(ConnectError::Io)?;
        let (mut session, info) = ClientSession::connect(
            stream,
            ClientOptions {
                password: options.password.clone(),
                username: options.username.clone(),
                tls: Some(TlsOptions {
                    server_name: host_of(addr).to_owned(),
                    pins: options.pins.clone(),
                    extra_roots: options.extra_roots.clone(),
                }),
                // The same pins serve for an RSA-AES server key: both are
                // SHA-256 fingerprints the user confirmed.
                ra2: Some(rfb_tokio::Ra2Options {
                    server_name: host_of(addr).to_owned(),
                    pins: options.pins.clone(),
                }),
                // A server the user already pinned must keep encrypting: an
                // attacker on the path cannot talk us into plain VNC Auth.
                require_encryption: !options.pins.is_empty(),
                ..ClientOptions::default()
            },
        )
        .await?;
        let ServerInfo {
            width,
            height,
            name,
            ..
        } = info;
        let fb = Framebuffer::new(width, height)?;
        let mut decoder = Decoder::new(&PixelFormat::RGB888)?;
        let mut wanted = encodings.to_vec();
        if let Some(codec) = &options.video {
            decoder.set_video(std::sync::Arc::clone(codec));
            if !wanted.contains(&enc::OPEN_H264) {
                // H.264 first: a server that has it sends it, others use the rest.
                wanted.insert(0, enc::OPEN_H264);
            }
        }
        if options.continuous_updates {
            wanted.push(pseudo::CONTINUOUS_UPDATES);
        }
        if let Some(level) = options.compression {
            wanted.push(pseudo::COMPRESS_0 + i32::from(level.min(9)));
        }
        if let Some(quality) = options.quality {
            wanted.push(pseudo::JPEG_QUALITY_0 + i32::from(quality.min(9)));
            // A server sends JPEG only inside Tight, so Tight must come
            // first; the decoder may now read it.
            if let Some(at) = wanted.iter().position(|e| *e == enc::TIGHT) {
                wanted.remove(at);
                wanted.insert(0, enc::TIGHT);
            }
            decoder.allow_jpeg(true);
        }
        session
            .send(&ClientMessage::SetPixelFormat(PixelFormat::RGB888))
            .await?;
        session.send(&ClientMessage::SetEncodings(wanted)).await?;
        let mut screen = Self {
            session,
            decoder,
            fb,
            name: String::from_utf8_lossy(&name).into_owned(),
            stats: Collector::new(std::time::Instant::now()),
            screens: Vec::new(),
            clip: ExtClipboard::new(Role::Client),
            pings: VecDeque::new(),
            next_ping: 0,
            wants_continuous: options.continuous_updates,
            continuous: None,
        };
        screen.request_update(false).await?;
        Ok(screen)
    }

    /// The protocol version in use, such as `RFB 3.8`.
    #[must_use]
    pub fn protocol(&self) -> String {
        match self.session.version() {
            rfb_proto::ProtocolVersion::V3_3 => "RFB 3.3",
            rfb_proto::ProtocolVersion::V3_7 => "RFB 3.7",
            rfb_proto::ProtocolVersion::V3_8 => "RFB 3.8",
        }
        .to_owned()
    }

    /// How the session signed in and is protected, such as
    /// `VeNCrypt X509Vnc` or `VNC Auth`.
    #[must_use]
    pub fn security(&self) -> String {
        use rfb_proto::consts::{security, vencrypt};
        match self.session.security_type() {
            Some(security::NONE) => "None".to_owned(),
            Some(security::VNC_AUTH) => "VNC Auth".to_owned(),
            Some(security::APPLE_DH) => "Apple Remote Desktop".to_owned(),
            Some(security::VENCRYPT) => match self.session.vencrypt_subtype() {
                Some(vencrypt::X509_NONE) => "VeNCrypt X509None".to_owned(),
                Some(vencrypt::X509_VNC) => "VeNCrypt X509Vnc".to_owned(),
                Some(vencrypt::X509_PLAIN) => "VeNCrypt X509Plain".to_owned(),
                _ => "VeNCrypt".to_owned(),
            },
            Some(security::RA2) => "RSA-AES".to_owned(),
            Some(security::RA2_256) => "RSA-AES-256".to_owned(),
            Some(security::RA2NE) => "RSA-AES (unencrypted after sign-in)".to_owned(),
            Some(security::RA2NE_256) => "RSA-AES-256 (unencrypted after sign-in)".to_owned(),
            _ => "unknown".to_owned(),
        }
    }

    /// The remote desktop's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current picture.
    #[must_use]
    pub fn framebuffer(&self) -> &Framebuffer {
        &self.fb
    }

    /// Ask for the whole screen (`incremental == false`) or only changes.
    ///
    /// # Errors
    /// I/O errors.
    pub async fn request_update(&mut self, incremental: bool) -> Result<(), ConnectError> {
        if incremental && self.continuous.is_some() {
            // The server pushes changes by itself.
            return Ok(());
        }
        let rect = Rect {
            x: 0,
            y: 0,
            width: self.fb.width(),
            height: self.fb.height(),
        };
        self.session
            .send(&ClientMessage::FramebufferUpdateRequest { incremental, rect })
            .await?;
        Ok(())
    }

    /// Wait for the next thing worth reacting to. Decoding happens before
    /// this returns, so the picture is already up to date for a
    /// [`ScreenEvent::Damage`]. Cancel safe: dropping the future (for example
    /// in a `select!`) loses nothing.
    ///
    /// # Errors
    /// Session errors (including the server hanging up) and decode errors;
    /// after an error the connection is unusable.
    pub async fn next_event(&mut self) -> Result<ScreenEvent, ConnectError> {
        loop {
            match self.session.next_event().await? {
                ClientConnEvent::Rect { header, payload } => {
                    self.stats.record_rect(header.encoding, payload.len());
                    match self.decoder.decode(&mut self.fb, &header, &payload)? {
                        Damage::None => {}
                        Damage::Cursor(cursor) => return Ok(ScreenEvent::Cursor(cursor)),
                        Damage::Layout(layout) => {
                            if layout.status == 0 {
                                self.screens.clone_from(&layout.screens);
                            }
                            self.follow_size().await?;
                            return Ok(ScreenEvent::Layout(layout));
                        }
                        damage => {
                            if matches!(damage, Damage::Resized) {
                                self.follow_size().await?;
                            }
                            return Ok(ScreenEvent::Damage(damage));
                        }
                    }
                }
                ClientConnEvent::UpdateEnd => {
                    self.stats.record_update();
                    return Ok(ScreenEvent::UpdateEnd);
                }
                ClientConnEvent::Bell => return Ok(ScreenEvent::Bell),
                ClientConnEvent::CutText(text) => return Ok(ScreenEvent::CutText(text)),
                ClientConnEvent::ExtendedClipboard { flags, payload } => {
                    let reaction = self
                        .clip
                        .on_message(flags, &payload)
                        .map_err(ConnectError::Clipboard)?;
                    for (flags, payload) in reaction.send {
                        self.session
                            .send(&ClientMessage::ExtendedClipboard { flags, payload })
                            .await?;
                    }
                    if let Some(text) = reaction.received {
                        return Ok(ScreenEvent::Text(text));
                    }
                }
                ClientConnEvent::Fence { flags, payload } => {
                    if flags & fence::REQUEST != 0 {
                        // Answer at once, keeping only the flags we know.
                        let known = fence::BLOCK_BEFORE | fence::BLOCK_AFTER | fence::SYNC_NEXT;
                        self.session
                            .send(&ClientMessage::Fence {
                                flags: flags & known,
                                payload,
                            })
                            .await?;
                    } else if let Ok(seq) = <[u8; 8]>::try_from(payload.as_slice()) {
                        let seq = u64::from_be_bytes(seq);
                        // Older requests that were never answered are dropped.
                        while let Some((n, sent)) = self.pings.pop_front() {
                            if n == seq {
                                self.stats.record_rtt(sent.elapsed());
                                break;
                            }
                        }
                    }
                }
                ClientConnEvent::EndOfContinuousUpdates => {
                    // Either the server says it can push updates, or it has
                    // stopped after we switched them off.
                    if self.wants_continuous && self.continuous.is_none() {
                        self.enable_continuous().await?;
                    }
                }
                ClientConnEvent::Handshake(_)
                | ClientConnEvent::UpdateBegin { .. }
                | ClientConnEvent::ColourMap { .. } => {}
            }
        }
    }

    /// Switch continuous updates on for the whole picture.
    async fn enable_continuous(&mut self) -> Result<(), ConnectError> {
        let size = (self.fb.width(), self.fb.height());
        self.session
            .send(&ClientMessage::EnableContinuousUpdates {
                enable: true,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: size.0,
                    height: size.1,
                },
            })
            .await?;
        self.continuous = Some(size);
        Ok(())
    }

    /// The picture changed size: continuous updates must cover the new one.
    async fn follow_size(&mut self) -> Result<(), ConnectError> {
        if self
            .continuous
            .is_some_and(|size| size != (self.fb.width(), self.fb.height()))
        {
            self.enable_continuous().await?;
        }
        Ok(())
    }

    /// Wait for one complete `FramebufferUpdate`, decode it into the picture
    /// and return what changed. Not cancel safe: use [`RemoteScreen::next_event`]
    /// inside a `select!`.
    ///
    /// # Errors
    /// As [`RemoteScreen::next_event`].
    pub async fn next_update(&mut self) -> Result<Vec<Damage>, ConnectError> {
        let mut damage = Vec::new();
        loop {
            match self.next_event().await? {
                ScreenEvent::Damage(d) => damage.push(d),
                ScreenEvent::UpdateEnd => return Ok(damage),
                ScreenEvent::Cursor(_)
                | ScreenEvent::Layout(_)
                | ScreenEvent::Bell
                | ScreenEvent::CutText(_)
                | ScreenEvent::Text(_) => {}
            }
        }
    }

    /// Take a statistics reading: rates since the previous reading, totals,
    /// and the encoding mix since the connection started.
    pub fn sample_stats(&mut self, now: std::time::Instant) -> Snapshot {
        let mut reading = self.stats.sample(
            now,
            self.session.bytes_received(),
            self.session.bytes_sent(),
        );
        reading.ext_clipboard = self.clip.is_active();
        reading
    }

    /// Measure the round trip: send a Fence request that the server answers
    /// once it has handled everything sent before it. The result shows up in
    /// [`RemoteScreen::sample_stats`] after the answer has been read by
    /// [`RemoteScreen::next_event`]. Returns `false` without sending if the
    /// server has not shown Fence support or too many requests are still
    /// unanswered.
    ///
    /// # Errors
    /// I/O errors.
    pub async fn ping(&mut self) -> Result<bool, ConnectError> {
        if !self.session.conn().supports_fence() || self.pings.len() >= 4 {
            return Ok(false);
        }
        let seq = self.next_ping;
        self.next_ping += 1;
        self.pings.push_back((seq, Instant::now()));
        self.session
            .send(&ClientMessage::Fence {
                flags: fence::REQUEST | fence::BLOCK_BEFORE,
                payload: seq.to_be_bytes().to_vec(),
            })
            .await?;
        Ok(true)
    }

    /// Ask the server to resize the remote desktop to `width` x `height`
    /// (one screen). Returns `false` without sending if the server has not
    /// shown that it supports this. The answer arrives as a
    /// [`ScreenEvent::Layout`] with reason 1.
    ///
    /// # Errors
    /// I/O errors.
    pub async fn request_size(&mut self, width: u16, height: u16) -> Result<bool, ConnectError> {
        if !self.session.conn().supports_desktop_resize() {
            return Ok(false);
        }
        // Keep the identity (id, flags) of the first screen, as the spec asks
        // clients to do; a multi-screen layout collapses to one screen.
        let first = self.screens.first().copied().unwrap_or_default();
        let screen = Screen {
            x: 0,
            y: 0,
            width,
            height,
            ..first
        };
        self.session
            .send(&ClientMessage::SetDesktopSize {
                width,
                height,
                screens: vec![screen],
            })
            .await?;
        Ok(true)
    }

    /// Send a key press or release (an X11 keysym).
    ///
    /// # Errors
    /// I/O errors.
    pub async fn key(&mut self, keysym: u32, down: bool) -> Result<(), ConnectError> {
        Ok(self
            .session
            .send(&ClientMessage::KeyEvent { down, keysym })
            .await?)
    }

    /// Send the pointer position (remote framebuffer pixels) and button mask
    /// (bit 0 left, 1 middle, 2 right, 3 wheel up, 4 wheel down, ...).
    ///
    /// # Errors
    /// I/O errors.
    pub async fn pointer(&mut self, buttons: u8, x: u16, y: u16) -> Result<(), ConnectError> {
        Ok(self
            .session
            .send(&ClientMessage::PointerEvent { buttons, x, y })
            .await?)
    }

    /// Send local clipboard text: UTF-8 when the server supports Extended
    /// Clipboard, otherwise as Latin-1 `ClientCutText`.
    ///
    /// # Errors
    /// I/O errors.
    pub async fn send_clipboard(&mut self, text: &str) -> Result<ClipboardSent, ConnectError> {
        if self.clip.is_active() {
            let limit = rfb_proto::consts::limits::MAX_EXT_CLIPBOARD_INFLATED as usize;
            if clipboard::wire_size(text) > limit {
                return Ok(ClipboardSent::TooLarge { limit });
            }
            for (flags, payload) in self.clip.local_changed(text) {
                self.session
                    .send(&ClientMessage::ExtendedClipboard { flags, payload })
                    .await?;
            }
            return Ok(ClipboardSent::Utf8);
        }
        match crate::clipboard::to_remote(text) {
            Ok(out) => {
                self.cut_text(out.latin1).await?;
                Ok(ClipboardSent::Latin1 {
                    replaced: out.replaced,
                })
            }
            Err(crate::clipboard::ClipboardError::TooLarge { .. }) => Ok(ClipboardSent::TooLarge {
                limit: rfb_proto::consts::limits::MAX_CUT_TEXT_LEN as usize,
            }),
        }
    }

    /// Send clipboard text (Latin-1 as the legacy message requires).
    ///
    /// # Errors
    /// I/O errors.
    pub async fn cut_text(&mut self, latin1: Vec<u8>) -> Result<(), ConnectError> {
        Ok(self
            .session
            .send(&ClientMessage::ClientCutText(latin1))
            .await?)
    }
}

/// Something that happened on the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenEvent {
    /// The picture changed (already applied).
    Damage(Damage),
    /// The server announced its screen layout (also the answer to a resize
    /// request: check `reason` 1 and `status`). The picture is already resized.
    Layout(DesktopLayout),
    /// The cursor shape changed (an empty one means "hide the cursor").
    Cursor(Cursor),
    /// The server finished one `FramebufferUpdate`; request the next one.
    UpdateEnd,
    Bell,
    /// Clipboard text from the server (Latin-1 bytes).
    CutText(Vec<u8>),
    /// Clipboard text from the server through Extended Clipboard (UTF-8, line
    /// ends as the server sent them).
    Text(String),
}

/// How [`RemoteScreen::send_clipboard`] sent the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSent {
    /// As UTF-8 through Extended Clipboard (announced or provided).
    Utf8,
    /// As Latin-1 `ClientCutText`; `replaced` characters became `?`.
    Latin1 { replaced: usize },
    /// Too large for either path; nothing was sent.
    TooLarge { limit: usize },
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn the_certificate_name_is_the_host_without_the_port() {
        assert_eq!(host_of("example.org:5900"), "example.org");
        assert_eq!(host_of(" 10.0.0.5:5901 "), "10.0.0.5");
        assert_eq!(host_of("[::1]:5900"), "::1");
        assert_eq!(host_of("[fe80::1]"), "fe80::1");
        assert_eq!(
            host_of("::1"),
            "::1",
            "a bare IPv6 address has no port to strip"
        );
        assert_eq!(host_of("localhost"), "localhost");
    }
}
