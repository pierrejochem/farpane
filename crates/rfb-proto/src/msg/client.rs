//! Client-to-server messages (core set, RFB 3.8 section 7.5).

use alloc::vec::Vec;

use crate::{
    ProtoError,
    consts::{client_msg, limits},
    types::{PixelFormat, Rect, Screen},
    wire::Reader,
};

/// A client-to-server message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    SetPixelFormat(PixelFormat),
    SetEncodings(Vec<i32>),
    FramebufferUpdateRequest {
        incremental: bool,
        rect: Rect,
    },
    KeyEvent {
        down: bool,
        keysym: u32,
    },
    PointerEvent {
        buttons: u8,
        x: u16,
        y: u16,
    },
    /// Latin-1 text.
    ClientCutText(Vec<u8>),
    /// An Extended Clipboard message (negative length): `flags` and the
    /// payload after them, interpreted by `rfb-codec`'s clipboard module.
    ExtendedClipboard {
        flags: u32,
        payload: Vec<u8>,
    },
    /// `ClientFence`: a synchronisation request or the answer to the server's.
    Fence {
        flags: u32,
        payload: Vec<u8>,
    },
    /// `EnableContinuousUpdates`: while enabled the server sends updates for
    /// `rect` without waiting for requests. Only valid once the server has
    /// answered the `ContinuousUpdates` pseudo-encoding with
    /// `EndOfContinuousUpdates`.
    EnableContinuousUpdates {
        enable: bool,
        rect: Rect,
    },
    /// Ask for a new framebuffer size and screen layout. Only valid after an
    /// `ExtendedDesktopSize` rectangle was received.
    SetDesktopSize {
        width: u16,
        height: u16,
        screens: Vec<Screen>,
    },
}

impl ClientMessage {
    /// Parse one message from the start of `buf`; returns it with the number
    /// of bytes consumed. Nothing is consumed on error.
    ///
    /// # Errors
    /// [`ProtoError::NeedMore`] if `buf` holds a partial message,
    /// [`ProtoError::Malformed`] on an unknown message type or bad pixel
    /// format, [`ProtoError::LimitExceeded`] if a length field is too large.
    pub fn parse(buf: &[u8]) -> Result<(Self, usize), ProtoError> {
        let mut r = Reader::new(buf);
        let msg = match r.u8()? {
            client_msg::SET_PIXEL_FORMAT => {
                r.skip(3)?;
                Self::SetPixelFormat(PixelFormat::read(&mut r)?)
            }
            client_msg::SET_ENCODINGS => {
                r.skip(1)?;
                let n = r.u16()?;
                if n > limits::MAX_ENCODINGS {
                    return Err(ProtoError::LimitExceeded {
                        field: "SetEncodings count",
                        value: u64::from(n),
                        limit: u64::from(limits::MAX_ENCODINGS),
                    });
                }
                let mut encodings = Vec::with_capacity(usize::from(n));
                for _ in 0..n {
                    encodings.push(r.i32()?);
                }
                Self::SetEncodings(encodings)
            }
            client_msg::FRAMEBUFFER_UPDATE_REQUEST => Self::FramebufferUpdateRequest {
                incremental: r.u8()? != 0,
                rect: read_rect(&mut r)?,
            },
            client_msg::KEY_EVENT => {
                let down = r.u8()? != 0;
                r.skip(2)?;
                Self::KeyEvent {
                    down,
                    keysym: r.u32()?,
                }
            }
            client_msg::POINTER_EVENT => Self::PointerEvent {
                buttons: r.u8()?,
                x: r.u16()?,
                y: r.u16()?,
            },
            client_msg::CLIENT_CUT_TEXT => {
                r.skip(3)?;
                match super::read_cut_text(&mut r, "ClientCutText length")? {
                    super::CutText::Legacy(text) => Self::ClientCutText(text),
                    super::CutText::Extended { flags, payload } => {
                        Self::ExtendedClipboard { flags, payload }
                    }
                }
            }
            client_msg::CLIENT_FENCE => {
                let (flags, payload) = super::read_fence(&mut r)?;
                Self::Fence { flags, payload }
            }
            client_msg::ENABLE_CONTINUOUS_UPDATES => Self::EnableContinuousUpdates {
                enable: r.u8()? != 0,
                rect: read_rect(&mut r)?,
            },
            client_msg::SET_DESKTOP_SIZE => {
                r.skip(1)?;
                let (width, height) = (r.u16()?, r.u16()?);
                if width > limits::MAX_FB_DIMENSION || height > limits::MAX_FB_DIMENSION {
                    return Err(ProtoError::LimitExceeded {
                        field: "SetDesktopSize dimension",
                        value: u64::from(width.max(height)),
                        limit: u64::from(limits::MAX_FB_DIMENSION),
                    });
                }
                let count = r.u8()?;
                r.skip(1)?;
                let mut screens = Vec::with_capacity(usize::from(count));
                for _ in 0..count {
                    screens.push(Screen::read(&mut r)?);
                }
                Self::SetDesktopSize {
                    width,
                    height,
                    screens,
                }
            }
            _ => {
                return Err(ProtoError::Malformed {
                    ctx: "client message type",
                });
            }
        };
        Ok((msg, r.position()))
    }

    /// Append the wire form to `out`.
    ///
    /// # Errors
    /// [`ProtoError::LimitExceeded`] if a list or text is too long to encode.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        match self {
            Self::SetPixelFormat(pf) => {
                out.extend_from_slice(&[client_msg::SET_PIXEL_FORMAT, 0, 0, 0]);
                out.extend_from_slice(&pf.to_bytes());
            }
            Self::SetEncodings(encodings) => {
                let n = u16::try_from(encodings.len())
                    .ok()
                    .filter(|&n| n <= limits::MAX_ENCODINGS)
                    .ok_or(ProtoError::LimitExceeded {
                        field: "SetEncodings count",
                        value: encodings.len() as u64,
                        limit: u64::from(limits::MAX_ENCODINGS),
                    })?;
                out.extend_from_slice(&[client_msg::SET_ENCODINGS, 0]);
                out.extend_from_slice(&n.to_be_bytes());
                for e in encodings {
                    out.extend_from_slice(&e.to_be_bytes());
                }
            }
            Self::FramebufferUpdateRequest { incremental, rect } => {
                out.extend_from_slice(&[
                    client_msg::FRAMEBUFFER_UPDATE_REQUEST,
                    u8::from(*incremental),
                ]);
                write_rect(out, *rect);
            }
            Self::EnableContinuousUpdates { enable, rect } => {
                out.extend_from_slice(&[client_msg::ENABLE_CONTINUOUS_UPDATES, u8::from(*enable)]);
                write_rect(out, *rect);
            }
            Self::KeyEvent { down, keysym } => {
                out.extend_from_slice(&[client_msg::KEY_EVENT, u8::from(*down), 0, 0]);
                out.extend_from_slice(&keysym.to_be_bytes());
            }
            Self::PointerEvent { buttons, x, y } => {
                out.extend_from_slice(&[client_msg::POINTER_EVENT, *buttons]);
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
            }
            Self::ClientCutText(text) => {
                let len = u32::try_from(text.len())
                    .ok()
                    .filter(|&n| n <= limits::MAX_CUT_TEXT_LEN)
                    .ok_or(ProtoError::LimitExceeded {
                        field: "ClientCutText length",
                        value: text.len() as u64,
                        limit: u64::from(limits::MAX_CUT_TEXT_LEN),
                    })?;
                out.extend_from_slice(&[client_msg::CLIENT_CUT_TEXT, 0, 0, 0]);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(text);
            }
            Self::ExtendedClipboard { flags, payload } => super::write_extended_cut_text(
                out,
                client_msg::CLIENT_CUT_TEXT,
                *flags,
                payload,
                "ClientCutText length",
            )?,
            Self::Fence { flags, payload } => {
                super::write_fence(out, client_msg::CLIENT_FENCE, *flags, payload)?;
            }
            Self::SetDesktopSize {
                width,
                height,
                screens,
            } => {
                // `u8` already is the spec maximum (`MAX_SCREENS`, 255).
                let count = u8::try_from(screens.len())
                    .ok()
                    .ok_or(ProtoError::LimitExceeded {
                        field: "SetDesktopSize screens",
                        value: screens.len() as u64,
                        limit: u64::from(limits::MAX_SCREENS),
                    })?;
                if *width > limits::MAX_FB_DIMENSION || *height > limits::MAX_FB_DIMENSION {
                    return Err(ProtoError::LimitExceeded {
                        field: "SetDesktopSize dimension",
                        value: u64::from((*width).max(*height)),
                        limit: u64::from(limits::MAX_FB_DIMENSION),
                    });
                }
                out.extend_from_slice(&[client_msg::SET_DESKTOP_SIZE, 0]);
                out.extend_from_slice(&width.to_be_bytes());
                out.extend_from_slice(&height.to_be_bytes());
                out.extend_from_slice(&[count, 0]);
                for screen in screens {
                    screen.write(out);
                }
            }
        }
        Ok(())
    }
}

fn read_rect(r: &mut Reader<'_>) -> Result<Rect, ProtoError> {
    Ok(Rect {
        x: r.u16()?,
        y: r.u16()?,
        width: r.u16()?,
        height: r.u16()?,
    })
}

fn write_rect(out: &mut Vec<u8>, rect: Rect) {
    for v in [rect.x, rect.y, rect.width, rect.height] {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    // Golden vectors: (message, exact wire bytes). Layouts checked against
    // rfbproto.rst: SetPixelFormat, SetEncodings, FramebufferUpdateRequest,
    // KeyEvent, PointerEvent and ClientCutText sections (2026-10-16).
    fn vectors() -> Vec<(ClientMessage, Vec<u8>)> {
        let mut set_pf = vec![0, 0, 0, 0];
        set_pf.extend_from_slice(&PixelFormat::RGB888.to_bytes());
        vec![
            (ClientMessage::SetPixelFormat(PixelFormat::RGB888), set_pf),
            (
                ClientMessage::SetEncodings(vec![16, 5, 1, 0, -223]),
                vec![
                    2, 0, 0, 5, 0, 0, 0, 16, 0, 0, 0, 5, 0, 0, 0, 1, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF,
                    0x21,
                ],
            ),
            (
                ClientMessage::FramebufferUpdateRequest {
                    incremental: true,
                    rect: Rect {
                        x: 1,
                        y: 2,
                        width: 640,
                        height: 480,
                    },
                },
                vec![3, 1, 0, 1, 0, 2, 0x02, 0x80, 0x01, 0xE0],
            ),
            // rfbproto.rst, EnableContinuousUpdates: type 150, U8 enable,
            // U16 x, y, width, height.
            (
                ClientMessage::EnableContinuousUpdates {
                    enable: true,
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    },
                },
                vec![150, 1, 0, 0, 0, 0, 0x07, 0x80, 0x04, 0x38],
            ),
            (
                ClientMessage::EnableContinuousUpdates {
                    enable: false,
                    rect: Rect {
                        x: 5,
                        y: 6,
                        width: 7,
                        height: 8,
                    },
                },
                vec![150, 0, 0, 5, 0, 6, 0, 7, 0, 8],
            ),
            (
                ClientMessage::KeyEvent {
                    down: true,
                    keysym: 0xFF0D,
                },
                vec![4, 1, 0, 0, 0, 0, 0xFF, 0x0D],
            ),
            (
                ClientMessage::PointerEvent {
                    buttons: 0b101,
                    x: 100,
                    y: 200,
                },
                vec![5, 5, 0, 100, 0, 200],
            ),
            (
                ClientMessage::ClientCutText(b"hi\n".to_vec()),
                vec![6, 0, 0, 0, 0, 0, 0, 3, b'h', b'i', b'\n'],
            ),
            // Extended Clipboard `request` for text (bit 25) from the client.
            (
                ClientMessage::ExtendedClipboard {
                    flags: 0x0200_0001,
                    payload: vec![],
                },
                vec![6, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFC, 0x02, 0x00, 0x00, 0x01],
            ),
            // rfbproto.rst, ClientFence: type 248, 3 padding bytes, U32 flags,
            // U8 length, payload. A plain request that blocks before.
            (
                ClientMessage::Fence {
                    flags: 0x8000_0001,
                    payload: vec![0xDE, 0xAD],
                },
                vec![248, 0, 0, 0, 0x80, 0, 0, 1, 2, 0xDE, 0xAD],
            ),
            // rfbproto.rst, SetDesktopSize: type 251, pad, width, height,
            // number-of-screens, pad, then 16 bytes per screen.
            (
                ClientMessage::SetDesktopSize {
                    width: 1280,
                    height: 720,
                    screens: vec![Screen {
                        id: 7,
                        x: 0,
                        y: 0,
                        width: 1280,
                        height: 720,
                        flags: 0xA,
                    }],
                },
                vec![
                    251, 0, 0x05, 0x00, 0x02, 0xD0, 1, 0, // header
                    0, 0, 0, 7, 0, 0, 0, 0, 0x05, 0x00, 0x02, 0xD0, 0, 0, 0, 0x0A, // screen
                ],
            ),
        ]
    }

    #[test]
    fn golden_and_round_trip() {
        for (msg, bytes) in vectors() {
            let mut out = Vec::new();
            msg.write(&mut out).unwrap();
            assert_eq!(out, bytes, "serialise {msg:?}");
            assert_eq!(ClientMessage::parse(&bytes), Ok((msg, bytes.len())));
        }
    }

    #[test]
    fn split_input_needs_more_until_complete() {
        for (msg, bytes) in vectors() {
            for cut in 0..bytes.len() {
                assert!(
                    matches!(
                        ClientMessage::parse(&bytes[..cut]),
                        Err(ProtoError::NeedMore(n)) if n > 0
                    ),
                    "{msg:?} cut at {cut}"
                );
            }
        }
    }

    #[test]
    fn byte_by_byte_stream_yields_all_messages() {
        let mut stream = Vec::new();
        let mut expected = Vec::new();
        for (msg, bytes) in vectors() {
            stream.extend_from_slice(&bytes);
            expected.push(msg);
        }
        let (mut buf, mut got) = (Vec::new(), Vec::new());
        for b in stream {
            buf.push(b);
            if let Ok((msg, used)) = ClientMessage::parse(&buf) {
                got.push(msg);
                buf.drain(..used);
            }
        }
        assert_eq!(buf.len(), 0);
        assert_eq!(got, expected);
    }

    #[test]
    fn unknown_type_is_fatal() {
        assert!(matches!(
            ClientMessage::parse(&[99]),
            Err(ProtoError::Malformed { .. })
        ));
    }

    #[test]
    fn limits_are_enforced_before_waiting_for_data() {
        let n = limits::MAX_ENCODINGS + 1;
        let mut b = vec![2, 0];
        b.extend_from_slice(&n.to_be_bytes());
        assert!(matches!(
            ClientMessage::parse(&b),
            Err(ProtoError::LimitExceeded { .. })
        ));
        // A legacy length above the limit (the largest positive S32).
        let mut b = vec![6, 0, 0, 0];
        b.extend_from_slice(&i32::MAX.to_be_bytes());
        assert!(matches!(
            ClientMessage::parse(&b),
            Err(ProtoError::LimitExceeded { .. })
        ));
        // 0xFFFFFFFF is -1 as a signed length: an extended message too short
        // to hold its flags.
        let mut b = vec![6, 0, 0, 0];
        b.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            ClientMessage::parse(&b),
            Err(ProtoError::Malformed { .. })
        ));
    }
}
