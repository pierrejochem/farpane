use alloc::vec::Vec;

use bytes::{Buf, Bytes, BytesMut};

use crate::{
    PixelFormat, ProtoError, ProtocolVersion,
    consts::{limits, pseudo},
    framing,
    handshake::{ClientConfig, ClientEvent, ClientHandshake},
    msg::{
        client::ClientMessage,
        server::{RectHeader, ServerMessage},
    },
};

/// What the caller must act on. Events come out one per `poll` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientConnEvent {
    /// A handshake event: the VNC Auth challenge, or `ServerInit` which ends
    /// the handshake.
    Handshake(ClientEvent),
    /// A `FramebufferUpdate` starts; `rects` rectangles follow (`0xFFFF`
    /// means "until `LastRect`").
    UpdateBegin {
        rects: u16,
    },
    /// One rectangle with its complete payload, ready for `rfb-codec`.
    /// `DesktopSize` rectangles are reported too, after the connection has
    /// applied the new size. `LastRect` is not reported; it ends the update.
    Rect {
        header: RectHeader,
        payload: Bytes,
    },
    /// The update is complete.
    UpdateEnd,
    ColourMap {
        first: u16,
        colours: Vec<[u16; 3]>,
    },
    Bell,
    CutText(Vec<u8>),
    /// An Extended Clipboard message; see `rfb-codec`'s clipboard module.
    ExtendedClipboard {
        flags: u32,
        payload: Vec<u8>,
    },
    /// `ServerFence`. A request (`fence::REQUEST` set) must be answered with
    /// a `ClientFence` that echoes the payload and the flags the client
    /// understands; otherwise it is the answer to our own request.
    Fence {
        flags: u32,
        payload: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Handshake,
    /// Between messages.
    Idle,
    /// Inside a `FramebufferUpdate`; `remaining` rectangles are announced.
    Update {
        remaining: u16,
    },
    Failed,
}

/// Client side of a connection: the handshake, then the server-to-client
/// message loop with rectangle framing.
///
/// The pixel format used for framing is the one most recently *sent*. A
/// client must not send `SetPixelFormat` while an update request is
/// outstanding (spec, `SetPixelFormat`), so no update in the old format can
/// still be in flight.
#[derive(Debug)]
pub struct ClientConn {
    hs: ClientHandshake,
    state: State,
    width: u16,
    height: u16,
    pf: PixelFormat,
    /// An `ExtendedDesktopSize` rectangle has arrived, so the server supports
    /// `SetDesktopSize`.
    ext_desktop_seen: bool,
    fence_seen: bool,
}

impl ClientConn {
    #[must_use]
    pub fn new(cfg: ClientConfig) -> Self {
        Self {
            hs: ClientHandshake::new(cfg),
            state: State::Handshake,
            width: 0,
            height: 0,
            pf: PixelFormat::RGB888,
            ext_desktop_seen: false,
            fence_seen: false,
        }
    }

    /// The version in use, valid once the server's version was read.
    #[must_use]
    pub fn version(&self) -> ProtocolVersion {
        self.hs.version()
    }

    /// Framebuffer size: from `ServerInit`, updated by `DesktopSize`.
    #[must_use]
    pub fn framebuffer_size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The pixel format rectangles are expected in.
    #[must_use]
    pub fn pixel_format(&self) -> &PixelFormat {
        &self.pf
    }

    /// Whether the server has shown it supports `SetDesktopSize`.
    #[must_use]
    pub fn supports_desktop_resize(&self) -> bool {
        self.ext_desktop_seen
    }

    /// Whether the server has shown it supports the Fence extension (it sent
    /// a `ServerFence`).
    #[must_use]
    pub fn supports_fence(&self) -> bool {
        self.fence_seen
    }

    /// True once `ServerInit` was received.
    #[must_use]
    pub fn is_established(&self) -> bool {
        !matches!(self.state, State::Handshake | State::Failed)
    }

    /// The RSA-AES handshake finished (after [`ClientEvent::StartRa2`]).
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if none was requested.
    pub fn ra2_established(&mut self) -> Result<(), ProtoError> {
        self.hs.ra2_established()
    }

    /// The TLS handshake finished (after [`ClientEvent::StartTls`]).
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if no TLS upgrade was requested.
    pub fn tls_established(&mut self) -> Result<(), ProtoError> {
        self.hs.tls_established()
    }

    /// Send `X509Plain` credentials; see [`ClientHandshake::plain_credentials`].
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if out of order, [`ProtoError::LimitExceeded`]
    /// for overlong values.
    pub fn plain_credentials(
        &mut self,
        username: &[u8],
        password: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.hs.plain_credentials(username, password, out)
    }

    /// Answer a VNC Auth challenge; see [`ClientHandshake::vnc_auth_reply`].
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if no challenge is pending.
    pub fn vnc_auth_reply(
        &mut self,
        response: &[u8; 16],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.hs.vnc_auth_reply(response, out)
    }

    /// Take the next event out of `buf`, consuming what it used. Returns
    /// `Ok(None)` when more input is needed; call again until then. Bytes to
    /// send (handshake replies) are appended to `out`.
    ///
    /// # Errors
    /// Any [`ProtoError`] other than `NeedMore`. All are fatal and every later
    /// call fails too.
    pub fn poll(
        &mut self,
        buf: &mut BytesMut,
        out: &mut Vec<u8>,
    ) -> Result<Option<ClientConnEvent>, ProtoError> {
        let r = self.step(buf, out);
        if r.is_err() {
            self.state = State::Failed;
        }
        r
    }

    /// Serialise `msg` onto `out`.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] before the handshake is complete,
    /// [`ProtoError::LimitExceeded`] if the message is too large.
    pub fn send(&mut self, msg: &ClientMessage, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        if !self.is_established() {
            return Err(ProtoError::Malformed {
                ctx: "message sent before handshake finished",
            });
        }
        if matches!(msg, ClientMessage::SetDesktopSize { .. }) && !self.ext_desktop_seen {
            return Err(ProtoError::ExtensionNotConfirmed("SetDesktopSize"));
        }
        if matches!(msg, ClientMessage::Fence { .. }) && !self.fence_seen {
            return Err(ProtoError::ExtensionNotConfirmed("Fence"));
        }
        msg.write(out)?;
        if let ClientMessage::SetPixelFormat(pf) = msg {
            self.pf = *pf;
        }
        Ok(())
    }

    fn step(
        &mut self,
        buf: &mut BytesMut,
        out: &mut Vec<u8>,
    ) -> Result<Option<ClientConnEvent>, ProtoError> {
        match self.state {
            State::Handshake => {
                let (n, ev) = self.hs.poll(buf, out)?;
                buf.advance(n);
                let Some(ev) = ev else { return Ok(None) };
                // The server cannot have sent anything encrypted yet, so
                // anything still buffered was injected in the clear and would
                // be read as if it came through TLS (a STARTTLS injection).
                if matches!(
                    ev,
                    ClientEvent::StartTls { .. } | ClientEvent::StartRa2 { .. }
                ) && !buf.is_empty()
                {
                    return Err(ProtoError::Malformed {
                        ctx: "data received before the encryption handshake",
                    });
                }
                if let ClientEvent::ServerInit {
                    width,
                    height,
                    pixel_format,
                    ..
                } = &ev
                {
                    (self.width, self.height, self.pf) = (*width, *height, *pixel_format);
                    self.state = State::Idle;
                }
                Ok(Some(ClientConnEvent::Handshake(ev)))
            }
            State::Idle => self.message(buf),
            State::Update { remaining: 0 } => {
                self.state = State::Idle;
                Ok(Some(ClientConnEvent::UpdateEnd))
            }
            State::Update { remaining } => self.rect(buf, remaining),
            State::Failed => Err(ProtoError::Malformed {
                ctx: "connection already failed",
            }),
        }
    }

    fn message(&mut self, buf: &mut BytesMut) -> Result<Option<ClientConnEvent>, ProtoError> {
        let (msg, used) = match ServerMessage::parse(buf) {
            Ok(v) => v,
            Err(ProtoError::NeedMore(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        buf.advance(used);
        Ok(Some(match msg {
            ServerMessage::UpdateBegin { rects } => {
                self.state = State::Update { remaining: rects };
                ClientConnEvent::UpdateBegin { rects }
            }
            ServerMessage::SetColourMapEntries { first, colours } => {
                ClientConnEvent::ColourMap { first, colours }
            }
            ServerMessage::Bell => ClientConnEvent::Bell,
            ServerMessage::ServerCutText(t) => ClientConnEvent::CutText(t),
            ServerMessage::ExtendedClipboard { flags, payload } => {
                ClientConnEvent::ExtendedClipboard { flags, payload }
            }
            ServerMessage::Fence { flags, payload } => {
                self.fence_seen = true;
                ClientConnEvent::Fence { flags, payload }
            }
        }))
    }

    fn rect(
        &mut self,
        buf: &mut BytesMut,
        remaining: u16,
    ) -> Result<Option<ClientConnEvent>, ProtoError> {
        let header = match RectHeader::parse(buf) {
            Ok(h) => h,
            Err(ProtoError::NeedMore(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if header.encoding == pseudo::LAST_RECT {
            buf.advance(RectHeader::LEN);
            self.state = State::Idle;
            return Ok(Some(ClientConnEvent::UpdateEnd));
        }
        if !is_pseudo(header.encoding) && !header.rect.fits(self.width, self.height) {
            return Err(ProtoError::Malformed {
                ctx: "rectangle outside framebuffer",
            });
        }
        let len = match framing::frame(&header, &self.pf, &buf[RectHeader::LEN..]) {
            Ok(n) => n,
            Err(ProtoError::NeedMore(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        buf.advance(RectHeader::LEN);
        let payload = buf.split_to(len).freeze();
        if header.encoding == pseudo::DESKTOP_SIZE {
            let (w, h) = (header.rect.width, header.rect.height);
            if w > limits::MAX_FB_DIMENSION || h > limits::MAX_FB_DIMENSION {
                return Err(ProtoError::LimitExceeded {
                    field: "framebuffer dimension",
                    value: u64::from(w.max(h)),
                    limit: u64::from(limits::MAX_FB_DIMENSION),
                });
            }
            (self.width, self.height) = (w, h);
        }
        if header.encoding == pseudo::EXT_DESKTOP_SIZE {
            self.ext_desktop_seen = true;
            // x is the reason, y the status: only status 0 carries a new size
            // (after a refusal the size fields are undefined).
            if header.rect.y == 0 {
                let (w, h) = (header.rect.width, header.rect.height);
                if w > limits::MAX_FB_DIMENSION || h > limits::MAX_FB_DIMENSION {
                    return Err(ProtoError::LimitExceeded {
                        field: "framebuffer dimension",
                        value: u64::from(w.max(h)),
                        limit: u64::from(limits::MAX_FB_DIMENSION),
                    });
                }
                (self.width, self.height) = (w, h);
            }
        }
        self.state = State::Update {
            remaining: remaining - 1,
        };
        Ok(Some(ClientConnEvent::Rect { header, payload }))
    }
}

/// Pseudo-encodings are the negative numbers; their rectangles do not describe
/// framebuffer pixels, so the bounds check does not apply.
fn is_pseudo(encoding: i32) -> bool {
    encoding < 0
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::{
        consts::{enc, security},
        handshake::{ServerConfig, ServerEvent, ServerHandshake},
        types::Rect,
    };

    const W: u16 = 100;
    const H: u16 = 50;

    fn hdr(encoding: i32, x: u16, y: u16, w: u16, h: u16) -> RectHeader {
        RectHeader {
            rect: Rect {
                x,
                y,
                width: w,
                height: h,
            },
            encoding,
        }
    }

    fn msg(m: &ServerMessage) -> Vec<u8> {
        let mut v = Vec::new();
        m.write(&mut v).unwrap();
        v
    }

    /// A client that completed a 3.8 None handshake against a real
    /// `ServerHandshake` (framebuffer `W` x `H`, 32 bpp). Returns it with
    /// `tail`, the bytes the test then feeds as server traffic.
    fn session(tail: &[u8]) -> (ClientConn, Vec<u8>) {
        let mut s_out = Vec::new();
        let mut srv = ServerHandshake::new(
            ServerConfig {
                max_version: ProtocolVersion::V3_8,
                security: vec![security::NONE],
                ..ServerConfig::default()
            },
            &mut s_out,
        );
        let mut cli = ClientConn::new(ClientConfig::default());
        let mut c_out = Vec::new();
        let mut c_in = BytesMut::new();
        let mut s_in = BytesMut::new();
        for _ in 0..16 {
            c_in.extend_from_slice(&s_out);
            s_out.clear();
            while cli.poll(&mut c_in, &mut c_out).unwrap().is_some() {}
            s_in.extend_from_slice(&c_out);
            c_out.clear();
            let (n, ev) = srv.poll(&s_in, &mut s_out).unwrap();
            s_in.advance(n);
            if let Some(ServerEvent::ClientInit { .. }) = ev {
                srv.server_init(W, H, &PixelFormat::RGB888, b"test", &mut s_out)
                    .unwrap();
            }
            if cli.is_established() {
                break;
            }
        }
        assert!(cli.is_established());
        (cli, tail.to_vec())
    }

    /// Feed `bytes` in `chunk`-sized pieces and collect every event.
    fn collect(
        cli: &mut ClientConn,
        bytes: &[u8],
        chunk: usize,
    ) -> Result<Vec<ClientConnEvent>, ProtoError> {
        let mut buf = BytesMut::new();
        let mut events = Vec::new();
        let mut out = Vec::new();
        for part in bytes.chunks(chunk) {
            buf.extend_from_slice(part);
            while let Some(ev) = cli.poll(&mut buf, &mut out)? {
                events.push(ev);
            }
        }
        assert_eq!(buf.len(), 0, "all input consumed");
        Ok(events)
    }

    #[test]
    fn full_update_in_any_chunking() {
        let raw = vec![7u8; 2 * 3 * 4];
        let mut tail = Vec::new();
        tail.extend(msg(&ServerMessage::Bell));
        tail.extend(msg(&ServerMessage::UpdateBegin { rects: 2 }));
        tail.extend_from_slice(&hdr(enc::RAW, 1, 1, 2, 3).to_bytes());
        tail.extend_from_slice(&raw);
        tail.extend_from_slice(&hdr(enc::COPY_RECT, 10, 10, 5, 5).to_bytes());
        tail.extend_from_slice(&[0, 1, 0, 2]);
        tail.extend(msg(&ServerMessage::ServerCutText(b"hi".to_vec())));
        for chunk in [usize::MAX, 1, 3, 7] {
            let (mut cli, tail) = session(&tail);
            let ev = collect(&mut cli, &tail, chunk).unwrap();
            assert_eq!(
                ev,
                vec![
                    ClientConnEvent::Bell,
                    ClientConnEvent::UpdateBegin { rects: 2 },
                    ClientConnEvent::Rect {
                        header: hdr(enc::RAW, 1, 1, 2, 3),
                        payload: Bytes::from(raw.clone()),
                    },
                    ClientConnEvent::Rect {
                        header: hdr(enc::COPY_RECT, 10, 10, 5, 5),
                        payload: Bytes::from_static(&[0, 1, 0, 2]),
                    },
                    ClientConnEvent::UpdateEnd,
                    ClientConnEvent::CutText(b"hi".to_vec()),
                ],
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn empty_update_begins_and_ends() {
        let (mut cli, tail) = session(&msg(&ServerMessage::UpdateBegin { rects: 0 }));
        assert_eq!(
            collect(&mut cli, &tail, 1).unwrap(),
            vec![
                ClientConnEvent::UpdateBegin { rects: 0 },
                ClientConnEvent::UpdateEnd
            ]
        );
    }

    #[test]
    fn last_rect_ends_an_unbounded_update() {
        let mut tail = msg(&ServerMessage::UpdateBegin { rects: 0xFFFF });
        tail.extend_from_slice(&hdr(enc::COPY_RECT, 0, 0, 1, 1).to_bytes());
        tail.extend_from_slice(&[0, 0, 0, 0]);
        tail.extend_from_slice(&hdr(pseudo::LAST_RECT, 0, 0, 0, 0).to_bytes());
        tail.extend(msg(&ServerMessage::Bell));
        let (mut cli, tail) = session(&tail);
        let ev = collect(&mut cli, &tail, 1).unwrap();
        assert_eq!(ev.len(), 4, "{ev:?}");
        assert_eq!(ev[2], ClientConnEvent::UpdateEnd);
        assert_eq!(ev[3], ClientConnEvent::Bell);
    }

    #[test]
    fn rects_must_fit_but_pseudo_rects_are_exempt() {
        let mut tail = msg(&ServerMessage::UpdateBegin { rects: 1 });
        tail.extend_from_slice(&hdr(enc::COPY_RECT, 99, 0, 2, 1).to_bytes());
        tail.extend_from_slice(&[0; 4]);
        let (mut cli, tail) = session(&tail);
        assert!(matches!(
            collect(&mut cli, &tail, 4),
            Err(ProtoError::Malformed { .. })
        ));

        // Cursor: x, y are the hotspot and w x h is the cursor size.
        let mut tail = msg(&ServerMessage::UpdateBegin { rects: 1 });
        tail.extend_from_slice(&hdr(pseudo::CURSOR, 500, 500, 2, 2).to_bytes());
        tail.extend_from_slice(&[0; 2 * 2 * 4 + 2]);
        let (mut cli, tail) = session(&tail);
        assert_eq!(collect(&mut cli, &tail, 5).unwrap().len(), 3);
    }

    #[test]
    fn desktop_size_changes_the_bounds() {
        let mut tail = msg(&ServerMessage::UpdateBegin { rects: 2 });
        tail.extend_from_slice(&hdr(pseudo::DESKTOP_SIZE, 0, 0, 200, 100).to_bytes());
        tail.extend_from_slice(&hdr(enc::COPY_RECT, 150, 60, 10, 10).to_bytes());
        tail.extend_from_slice(&[0; 4]);
        let (mut cli, tail) = session(&tail);
        assert_eq!(collect(&mut cli, &tail, 1).unwrap().len(), 4);
        assert_eq!(cli.framebuffer_size(), (200, 100));

        let mut tail = msg(&ServerMessage::UpdateBegin { rects: 1 });
        tail.extend_from_slice(&hdr(pseudo::DESKTOP_SIZE, 0, 0, 60_000, 10).to_bytes());
        let (mut cli, tail) = session(&tail);
        assert!(matches!(
            collect(&mut cli, &tail, 12),
            Err(ProtoError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn set_pixel_format_changes_raw_framing() {
        let (mut cli, _) = session(&[]);
        let pf16 = PixelFormat {
            bits_per_pixel: 16,
            depth: 16,
            ..PixelFormat::RGB888
        };
        let mut sent = Vec::new();
        cli.send(&ClientMessage::SetPixelFormat(pf16), &mut sent)
            .unwrap();
        assert_eq!(cli.pixel_format(), &pf16);
        let mut tail = msg(&ServerMessage::UpdateBegin { rects: 1 });
        tail.extend_from_slice(&hdr(enc::RAW, 0, 0, 4, 2).to_bytes());
        tail.extend_from_slice(&[1; 4 * 2 * 2]);
        let ev = collect(&mut cli, &tail, 1).unwrap();
        assert!(matches!(&ev[1], ClientConnEvent::Rect { payload, .. } if payload.len() == 16));
    }

    #[test]
    fn send_before_handshake_is_refused() {
        let mut cli = ClientConn::new(ClientConfig::default());
        let m = ClientMessage::PointerEvent {
            buttons: 0,
            x: 1,
            y: 1,
        };
        assert!(cli.send(&m, &mut Vec::new()).is_err());
    }

    #[test]
    fn errors_are_fatal_and_sticky() {
        let (mut cli, _) = session(&[]);
        let mut buf = BytesMut::from(&[99u8][..]);
        assert!(cli.poll(&mut buf, &mut Vec::new()).is_err());
        assert!(!cli.is_established());
        let mut buf = BytesMut::from(&msg(&ServerMessage::Bell)[..]);
        assert!(cli.poll(&mut buf, &mut Vec::new()).is_err());

        let mut tail = msg(&ServerMessage::UpdateBegin { rects: 1 });
        tail.extend_from_slice(&hdr(enc::ZYWRLE, 0, 0, 1, 1).to_bytes());
        tail.extend_from_slice(&[0; 8]);
        let (mut cli, tail) = session(&tail);
        assert_eq!(
            collect(&mut cli, &tail, 7),
            Err(ProtoError::UnsupportedEncoding(enc::ZYWRLE))
        );
    }
}
