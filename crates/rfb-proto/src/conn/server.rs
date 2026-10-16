use alloc::vec::Vec;

use bytes::{Buf, BytesMut};

use crate::{
    PixelFormat, ProtoError, ProtocolVersion,
    consts::{enc, limits, pseudo},
    framing,
    handshake::{ServerConfig, ServerEvent, ServerHandshake},
    msg::{
        client::ClientMessage,
        server::{RectHeader, ServerMessage},
    },
};

/// What the caller must act on. Events come out one per `poll` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerConnEvent {
    /// A handshake event: act on it with [`ServerConn::send_challenge`],
    /// [`ServerConn::security_result`] or [`ServerConn::server_init`].
    Handshake(ServerEvent),
    /// A message from the client. `SetPixelFormat` and `SetEncodings` have
    /// already been applied to the connection's state.
    Message(ClientMessage),
}

/// One rectangle to send: its header and the already-encoded payload.
#[derive(Debug, Clone, Copy)]
pub struct EncodedRect<'a> {
    pub header: RectHeader,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Handshake,
    Established,
    Failed,
}

/// Server side of a connection: the handshake, then the client-to-server
/// message loop, and a checked way to send updates.
#[derive(Debug)]
pub struct ServerConn {
    hs: ServerHandshake,
    state: State,
    width: u16,
    height: u16,
    /// The format the client wants rectangles in.
    pf: PixelFormat,
    /// Encodings the client listed in its latest `SetEncodings`, in order.
    encodings: Vec<i32>,
}

impl ServerConn {
    /// Start a connection. The server speaks first, so the `ProtocolVersion`
    /// message is written to `out`.
    #[must_use]
    pub fn new(cfg: ServerConfig, out: &mut Vec<u8>) -> Self {
        Self {
            hs: ServerHandshake::new(cfg, out),
            state: State::Handshake,
            width: 0,
            height: 0,
            pf: PixelFormat::RGB888,
            encodings: Vec::new(),
        }
    }

    /// The version in use, valid once the client's version was read.
    #[must_use]
    pub fn version(&self) -> ProtocolVersion {
        self.hs.version()
    }

    /// Current framebuffer size: from `server_init`, updated when a
    /// `DesktopSize` rectangle is sent.
    #[must_use]
    pub fn framebuffer_size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The pixel format the client wants: from `server_init` until the client
    /// sends `SetPixelFormat`.
    #[must_use]
    pub fn client_pixel_format(&self) -> &PixelFormat {
        &self.pf
    }

    /// Encodings the client asked for, most preferred first.
    #[must_use]
    pub fn client_encodings(&self) -> &[i32] {
        &self.encodings
    }

    /// True once `server_init` was sent.
    #[must_use]
    pub fn is_established(&self) -> bool {
        self.state == State::Established
    }

    /// The TLS handshake finished (after [`ServerEvent::StartTls`]).
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if no TLS upgrade was requested.
    pub fn tls_established(&mut self, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        self.hs.tls_established(out)
    }

    /// Send the VNC Auth challenge; see [`ServerHandshake::send_challenge`].
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if called out of order.
    pub fn send_challenge(
        &mut self,
        challenge: &[u8; 16],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.hs.send_challenge(challenge, out)
    }

    /// Send the `SecurityResult`; see [`ServerHandshake::security_result`].
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if called out of order,
    /// [`ProtoError::LimitExceeded`] if `reason` is too long.
    pub fn security_result(
        &mut self,
        ok: bool,
        reason: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.hs.security_result(ok, reason, out)
    }

    /// Send `ServerInit`, which ends the handshake. `pixel_format` is the
    /// server's native format and the client's until it sends `SetPixelFormat`.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if called out of order,
    /// [`ProtoError::LimitExceeded`] for a too-large framebuffer or name.
    pub fn server_init(
        &mut self,
        width: u16,
        height: u16,
        pixel_format: &PixelFormat,
        name: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.hs
            .server_init(width, height, pixel_format, name, out)?;
        (self.width, self.height, self.pf) = (width, height, *pixel_format);
        self.state = State::Established;
        Ok(())
    }

    /// Take the next event out of `buf`, consuming what it used. Returns
    /// `Ok(None)` when more input is needed; call again until then. Handshake
    /// bytes to send are appended to `out`.
    ///
    /// # Errors
    /// Any [`ProtoError`] other than `NeedMore`. All are fatal and every later
    /// call fails too.
    pub fn poll(
        &mut self,
        buf: &mut BytesMut,
        out: &mut Vec<u8>,
    ) -> Result<Option<ServerConnEvent>, ProtoError> {
        let r = self.step(buf, out);
        if r.is_err() {
            self.state = State::Failed;
        }
        r
    }

    fn step(
        &mut self,
        buf: &mut BytesMut,
        out: &mut Vec<u8>,
    ) -> Result<Option<ServerConnEvent>, ProtoError> {
        match self.state {
            State::Handshake => {
                let (n, ev) = self.hs.poll(buf, out)?;
                buf.advance(n);
                // The client sends its TLS hello only after our go-ahead, so
                // plaintext still buffered at this point was injected.
                if matches!(ev, Some(ServerEvent::StartTls { .. })) && !buf.is_empty() {
                    return Err(ProtoError::Malformed {
                        ctx: "data received before the TLS handshake",
                    });
                }
                Ok(ev.map(ServerConnEvent::Handshake))
            }
            State::Established => {
                let (msg, used) = match ClientMessage::parse(buf) {
                    Ok(v) => v,
                    Err(ProtoError::NeedMore(_)) => return Ok(None),
                    Err(e) => return Err(e),
                };
                buf.advance(used);
                match &msg {
                    ClientMessage::SetPixelFormat(pf) => self.pf = *pf,
                    ClientMessage::SetEncodings(list) => self.encodings.clone_from(list),
                    _ => {}
                }
                Ok(Some(ServerConnEvent::Message(msg)))
            }
            State::Failed => Err(ProtoError::Malformed {
                ctx: "connection already failed",
            }),
        }
    }

    /// Send a `FramebufferUpdate` of already-encoded rectangles.
    ///
    /// Each rectangle is checked before anything is written, because a bad one
    /// would desynchronise the client for good:
    /// - the encoding is Raw or one the client asked for;
    /// - a non-pseudo rectangle lies inside the framebuffer;
    /// - the payload frames to exactly its own length in the client's pixel
    ///   format.
    ///
    /// A `DesktopSize` rectangle changes the framebuffer bounds for the
    /// rectangles after it.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] before the handshake is complete or for a
    /// bad rectangle, [`ProtoError::EncodingNotRequested`],
    /// [`ProtoError::LimitExceeded`] for more than 65,535 rectangles.
    /// On error nothing is appended to `out` and the state is unchanged.
    pub fn send_update(
        &mut self,
        rects: &[EncodedRect<'_>],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.require_established("update sent before handshake finished")?;
        let count = u16::try_from(rects.len()).map_err(|_| ProtoError::LimitExceeded {
            field: "rectangles per update",
            value: rects.len() as u64,
            limit: u64::from(u16::MAX),
        })?;
        let (mut width, mut height) = (self.width, self.height);
        for r in rects {
            let enc = r.header.encoding;
            if enc != enc::RAW && !self.encodings.contains(&enc) {
                return Err(ProtoError::EncodingNotRequested(enc));
            }
            if enc >= 0 && !r.header.rect.fits(width, height) {
                return Err(ProtoError::Malformed {
                    ctx: "rectangle outside framebuffer",
                });
            }
            if enc == pseudo::LAST_RECT {
                return Err(ProtoError::Malformed {
                    ctx: "LastRect in a counted update",
                });
            }
            if framing::frame(&r.header, &self.pf, r.payload) != Ok(r.payload.len()) {
                return Err(ProtoError::Malformed {
                    ctx: "rectangle payload does not match its encoding",
                });
            }
            // ExtendedDesktopSize: y is the status; only 0 carries a size.
            if enc == pseudo::DESKTOP_SIZE
                || (enc == pseudo::EXT_DESKTOP_SIZE && r.header.rect.y == 0)
            {
                let (w, h) = (r.header.rect.width, r.header.rect.height);
                if w > limits::MAX_FB_DIMENSION || h > limits::MAX_FB_DIMENSION {
                    return Err(ProtoError::LimitExceeded {
                        field: "framebuffer dimension",
                        value: u64::from(w.max(h)),
                        limit: u64::from(limits::MAX_FB_DIMENSION),
                    });
                }
                (width, height) = (w, h);
            }
        }
        ServerMessage::UpdateBegin { rects: count }.write(out)?;
        for r in rects {
            out.extend_from_slice(&r.header.to_bytes());
            out.extend_from_slice(r.payload);
        }
        (self.width, self.height) = (width, height);
        Ok(())
    }

    /// Send a message other than a `FramebufferUpdate` (`Bell`,
    /// `ServerCutText`, `SetColourMapEntries`).
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] before the handshake is complete or for
    /// `UpdateBegin` (use [`ServerConn::send_update`]),
    /// [`ProtoError::LimitExceeded`] if the message is too large.
    pub fn send(&mut self, msg: &ServerMessage, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        self.require_established("message sent before handshake finished")?;
        if matches!(msg, ServerMessage::UpdateBegin { .. }) {
            return Err(ProtoError::Malformed {
                ctx: "use send_update for FramebufferUpdate",
            });
        }
        msg.write(out)
    }

    fn require_established(&self, ctx: &'static str) -> Result<(), ProtoError> {
        if self.is_established() {
            Ok(())
        } else {
            Err(ProtoError::Malformed { ctx })
        }
    }
}
