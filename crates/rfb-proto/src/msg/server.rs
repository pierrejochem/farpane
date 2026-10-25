//! Server-to-client messages (core set, RFB 3.8 section 7.6).
//!
//! `FramebufferUpdate` is split: this module parses the message header and
//! each rectangle header. Finding the end of a rectangle payload is the job of
//! the framers (milestone M2).

use alloc::vec::Vec;

use crate::{
    ProtoError,
    consts::{limits, server_msg},
    types::Rect,
    wire::Reader,
};

/// Header of one rectangle inside a `FramebufferUpdate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RectHeader {
    pub rect: Rect,
    pub encoding: i32,
}

impl RectHeader {
    /// Length on the wire.
    pub const LEN: usize = 12;

    /// # Errors
    /// [`ProtoError::NeedMore`] on short input.
    pub fn parse(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(buf);
        Ok(Self {
            rect: Rect {
                x: r.u16()?,
                y: r.u16()?,
                width: r.u16()?,
                height: r.u16()?,
            },
            encoding: r.i32()?,
        })
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0..2].copy_from_slice(&self.rect.x.to_be_bytes());
        b[2..4].copy_from_slice(&self.rect.y.to_be_bytes());
        b[4..6].copy_from_slice(&self.rect.width.to_be_bytes());
        b[6..8].copy_from_slice(&self.rect.height.to_be_bytes());
        b[8..12].copy_from_slice(&self.encoding.to_be_bytes());
        b
    }
}

/// A server-to-client message. Rectangle headers and payloads follow
/// [`ServerMessage::UpdateBegin`] and are read separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    /// Header of a `FramebufferUpdate`: the number of rectangles that follow.
    UpdateBegin {
        rects: u16,
    },
    SetColourMapEntries {
        first: u16,
        colours: Vec<[u16; 3]>,
    },
    Bell,
    /// Latin-1 text.
    ServerCutText(Vec<u8>),
    /// An Extended Clipboard message (negative length): `flags` and the
    /// payload after them, interpreted by `rfb-codec`'s clipboard module.
    ExtendedClipboard {
        flags: u32,
        payload: Vec<u8>,
    },
    /// `EndOfContinuousUpdates`: the server supports `ContinuousUpdates`
    /// (sent once after the client asked for the pseudo-encoding), and, after
    /// the client disabled them, that the last continuous update was sent.
    EndOfContinuousUpdates,
    /// `ServerFence`: a synchronisation request or the answer to ours.
    Fence {
        flags: u32,
        payload: Vec<u8>,
    },
}

impl ServerMessage {
    /// Parse one message from the start of `buf`; returns it with the number
    /// of bytes consumed.
    ///
    /// # Errors
    /// [`ProtoError::NeedMore`] on a partial message,
    /// [`ProtoError::Malformed`] on an unknown message type,
    /// [`ProtoError::LimitExceeded`] if the cut text is too long.
    pub fn parse(buf: &[u8]) -> Result<(Self, usize), ProtoError> {
        let mut r = Reader::new(buf);
        let msg = match r.u8()? {
            server_msg::FRAMEBUFFER_UPDATE => {
                r.skip(1)?;
                Self::UpdateBegin { rects: r.u16()? }
            }
            server_msg::SET_COLOUR_MAP_ENTRIES => {
                r.skip(1)?;
                let first = r.u16()?;
                let n = r.u16()?;
                let mut colours = Vec::with_capacity(usize::from(n));
                for _ in 0..n {
                    colours.push([r.u16()?, r.u16()?, r.u16()?]);
                }
                Self::SetColourMapEntries { first, colours }
            }
            server_msg::BELL => Self::Bell,
            server_msg::SERVER_CUT_TEXT => {
                r.skip(3)?;
                match super::read_cut_text(&mut r, "ServerCutText length")? {
                    super::CutText::Legacy(text) => Self::ServerCutText(text),
                    super::CutText::Extended { flags, payload } => {
                        Self::ExtendedClipboard { flags, payload }
                    }
                }
            }
            server_msg::END_OF_CONTINUOUS_UPDATES => Self::EndOfContinuousUpdates,
            server_msg::SERVER_FENCE => {
                let (flags, payload) = super::read_fence(&mut r)?;
                Self::Fence { flags, payload }
            }
            _ => {
                return Err(ProtoError::Malformed {
                    ctx: "server message type",
                });
            }
        };
        Ok((msg, r.position()))
    }

    /// Append the wire form to `out`.
    ///
    /// # Errors
    /// [`ProtoError::LimitExceeded`] if the cut text or colour list is too
    /// long to encode.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        match self {
            Self::UpdateBegin { rects } => {
                out.extend_from_slice(&[server_msg::FRAMEBUFFER_UPDATE, 0]);
                out.extend_from_slice(&rects.to_be_bytes());
            }
            Self::SetColourMapEntries { first, colours } => {
                let n = u16::try_from(colours.len()).map_err(|_| ProtoError::LimitExceeded {
                    field: "SetColourMapEntries count",
                    value: colours.len() as u64,
                    limit: u64::from(u16::MAX),
                })?;
                out.extend_from_slice(&[server_msg::SET_COLOUR_MAP_ENTRIES, 0]);
                out.extend_from_slice(&first.to_be_bytes());
                out.extend_from_slice(&n.to_be_bytes());
                for c in colours {
                    for v in c {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                }
            }
            Self::Bell => out.push(server_msg::BELL),
            Self::ServerCutText(text) => {
                let len = u32::try_from(text.len())
                    .ok()
                    .filter(|&n| n <= limits::MAX_CUT_TEXT_LEN)
                    .ok_or(ProtoError::LimitExceeded {
                        field: "ServerCutText length",
                        value: text.len() as u64,
                        limit: u64::from(limits::MAX_CUT_TEXT_LEN),
                    })?;
                out.extend_from_slice(&[server_msg::SERVER_CUT_TEXT, 0, 0, 0]);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(text);
            }
            Self::ExtendedClipboard { flags, payload } => super::write_extended_cut_text(
                out,
                server_msg::SERVER_CUT_TEXT,
                *flags,
                payload,
                "ServerCutText length",
            )?,
            Self::EndOfContinuousUpdates => out.push(server_msg::END_OF_CONTINUOUS_UPDATES),
            Self::Fence { flags, payload } => {
                super::write_fence(out, server_msg::SERVER_FENCE, *flags, payload)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    // Golden vectors checked against rfbproto.rst: FramebufferUpdate,
    // SetColourMapEntries, Bell and ServerCutText sections (2026-10-16).
    fn vectors() -> Vec<(ServerMessage, Vec<u8>)> {
        vec![
            (ServerMessage::UpdateBegin { rects: 3 }, vec![0, 0, 0, 3]),
            (
                ServerMessage::SetColourMapEntries {
                    first: 1,
                    colours: vec![[0xFFFF, 0, 0x0102]],
                },
                vec![1, 0, 0, 1, 0, 1, 0xFF, 0xFF, 0, 0, 1, 2],
            ),
            (ServerMessage::Bell, vec![2]),
            (
                ServerMessage::ServerCutText(b"ab".to_vec()),
                vec![3, 0, 0, 0, 0, 0, 0, 2, b'a', b'b'],
            ),
            // rfbproto.rst, Extended Clipboard: a negative length counts the
            // flags and everything after them. Here a `caps` message (bit 24)
            // for the text format (bit 0) plus actions, with one size entry.
            (
                ServerMessage::ExtendedClipboard {
                    flags: 0x1F00_0001,
                    payload: vec![0, 0, 0x10, 0],
                },
                vec![
                    3, 0, 0, 0, // type, padding
                    0xFF, 0xFF, 0xFF, 0xF8, // length = -8
                    0x1F, 0x00, 0x00, 0x01, // flags
                    0, 0, 0x10, 0, // sizes[text]
                ],
            ),
            // A `notify` (bit 27) with no data after the flags: length -4.
            (
                ServerMessage::ExtendedClipboard {
                    flags: 0x0800_0001,
                    payload: vec![],
                },
                vec![3, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFC, 0x08, 0x00, 0x00, 0x01],
            ),
            // rfbproto.rst, EndOfContinuousUpdates: the type byte alone.
            (ServerMessage::EndOfContinuousUpdates, vec![150]),
            // rfbproto.rst, ServerFence: type 248, 3 padding bytes, U32 flags
            // (Request | BlockBefore), U8 length, payload.
            (
                ServerMessage::Fence {
                    flags: 0x8000_0001,
                    payload: vec![1, 2, 3],
                },
                vec![248, 0, 0, 0, 0x80, 0, 0, 1, 3, 1, 2, 3],
            ),
            (
                ServerMessage::Fence {
                    flags: 0,
                    payload: vec![],
                },
                vec![248, 0, 0, 0, 0, 0, 0, 0, 0],
            ),
        ]
    }

    #[test]
    fn fence_payload_is_bounded() {
        let mut long = vec![248, 0, 0, 0, 0, 0, 0, 0, 65];
        long.extend_from_slice(&[0; 65]);
        assert!(matches!(
            ServerMessage::parse(&long),
            Err(ProtoError::LimitExceeded { .. })
        ));
        let mut out = Vec::new();
        let too_long = ServerMessage::Fence {
            flags: 0,
            payload: vec![0; 65],
        };
        assert!(too_long.write(&mut out).is_err());
        assert_eq!(out.len(), 0);
        let mut ok = vec![248, 0, 0, 0, 0, 0, 0, 0, 64];
        ok.extend_from_slice(&[7; 64]);
        assert!(ServerMessage::parse(&ok).is_ok());
    }

    #[test]
    fn golden_and_round_trip() {
        for (msg, bytes) in vectors() {
            let mut out = Vec::new();
            msg.write(&mut out).unwrap();
            assert_eq!(out, bytes, "serialise {msg:?}");
            assert_eq!(ServerMessage::parse(&bytes), Ok((msg, bytes.len())));
        }
    }

    #[test]
    fn split_input_needs_more_until_complete() {
        for (msg, bytes) in vectors() {
            for cut in 0..bytes.len() {
                assert!(
                    matches!(
                        ServerMessage::parse(&bytes[..cut]),
                        Err(ProtoError::NeedMore(n)) if n > 0
                    ),
                    "{msg:?} cut at {cut}"
                );
            }
        }
    }

    #[test]
    fn extended_clipboard_lengths_are_checked() {
        let with_len = |len: i32| {
            let mut b = vec![3, 0, 0, 0];
            b.extend_from_slice(&len.to_be_bytes());
            b.extend_from_slice(&[0; 8]);
            b
        };
        // Too small to hold the flags, and the most negative length of all.
        for len in [-1, -2, -3, i32::MIN] {
            assert!(
                matches!(
                    ServerMessage::parse(&with_len(len)),
                    Err(ProtoError::Malformed { .. } | ProtoError::LimitExceeded { .. })
                ),
                "{len}"
            );
        }
        // Above the inflated-size limit, refused before waiting for the data.
        let big = -i32::try_from(limits::MAX_EXT_CLIPBOARD_INFLATED + 1).unwrap();
        assert!(matches!(
            ServerMessage::parse(&with_len(big)),
            Err(ProtoError::LimitExceeded { .. })
        ));
        // Exactly the limit is accepted as "need more".
        let max = -i32::try_from(limits::MAX_EXT_CLIPBOARD_INFLATED).unwrap();
        assert!(matches!(
            ServerMessage::parse(&with_len(max)),
            Err(ProtoError::NeedMore(_))
        ));
        // Writing refuses what could not be parsed back.
        let mut out = Vec::new();
        let huge = ServerMessage::ExtendedClipboard {
            flags: 0,
            payload: vec![0; limits::MAX_EXT_CLIPBOARD_INFLATED as usize],
        };
        assert!(matches!(
            huge.write(&mut out),
            Err(ProtoError::LimitExceeded { .. })
        ));
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn unknown_type_is_fatal() {
        assert!(matches!(
            ServerMessage::parse(&[99]),
            Err(ProtoError::Malformed { .. })
        ));
    }

    #[test]
    fn rect_header_golden_and_round_trip() {
        let h = RectHeader {
            rect: Rect {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
            encoding: -223,
        };
        // rfbproto.rst, FramebufferUpdate: x, y, w, h (U16), encoding (S32).
        let bytes = [0, 1, 0, 2, 0, 3, 0, 4, 0xFF, 0xFF, 0xFF, 0x21];
        assert_eq!(h.to_bytes(), bytes);
        assert_eq!(RectHeader::parse(&bytes), Ok(h));
        assert_eq!(
            RectHeader::parse(&bytes[..11]),
            Err(ProtoError::NeedMore(1))
        );
    }
}
