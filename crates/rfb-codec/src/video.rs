//! H.264 ("Open H.264", encoding 50) without any H.264 code in this crate.
//!
//! Decoding and encoding H.264 is left to a [`VideoCodec`] supplied by the
//! application (a loaded `OpenH264` library, an OS encoder, a test double), so
//! this crate stays free of native code and of patent-encumbered binaries.
//! What lives here is the part that belongs to RFB: the message layout, the
//! per-rectangle decoder contexts and the reset flags.

#![allow(clippy::doc_markdown)] // OpenH264 is a name, not code

use std::{collections::HashMap, fmt, sync::Arc};

use rfb_proto::{consts::limits, types::Rect, wire::Reader};

use crate::DecodeError;

/// `ResetContext`: delete the context of this rectangle and start a new one.
pub const RESET_CONTEXT: u32 = 1 << 0;
/// `ResetAllContexts`: delete every context.
pub const RESET_ALL_CONTEXTS: u32 = 1 << 1;
/// The client must support this many contexts at once (rfbproto).
pub const MAX_CONTEXTS: usize = 64;

/// A decoded picture: `B, G, R, X` rows, `width * 4` bytes each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u16,
    pub height: u16,
    pub bgrx: Vec<u8>,
}

/// Decodes the H.264 stream of one context.
pub trait VideoDecoder: Send + fmt::Debug {
    /// Decode `data` (whole frames, Annex B byte stream). Returns the last
    /// picture, or `None` when the data did not complete one.
    ///
    /// # Errors
    /// A message when the decoder cannot make sense of the data.
    fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String>;
}

/// Encodes one stream: a context on the server.
pub trait VideoEncoder: Send + fmt::Debug {
    /// Encode a `width` x `height` picture of `B, G, R, X` rows (`stride`
    /// bytes apart) as one frame. The first frame, and any with `key` set, is
    /// an intra frame. The stream is Annex B, with parameter sets in front of
    /// every intra frame.
    ///
    /// # Errors
    /// A message when encoding fails.
    fn encode(
        &mut self,
        bgrx: &[u8],
        width: u16,
        height: u16,
        stride: usize,
        key: bool,
    ) -> Result<Vec<u8>, String>;
}

/// A source of H.264 decoders and encoders.
pub trait VideoCodec: Send + Sync + fmt::Debug {
    /// A new decoder, with no state.
    ///
    /// # Errors
    /// A message when none can be made.
    fn decoder(&self) -> Result<Box<dyn VideoDecoder>, String>;

    /// A new encoder for pictures of at least `width` x `height`.
    ///
    /// # Errors
    /// A message when none can be made.
    fn encoder(&self, width: u16, height: u16) -> Result<Box<dyn VideoEncoder>, String>;
}

/// The decoder contexts of a connection, one per rectangle.
#[derive(Debug)]
pub struct Contexts {
    codec: Arc<dyn VideoCodec>,
    decoders: HashMap<(u16, u16, u16, u16), Box<dyn VideoDecoder>>,
}

fn key(rect: Rect) -> (u16, u16, u16, u16) {
    (rect.x, rect.y, rect.width, rect.height)
}

impl Contexts {
    #[must_use]
    pub fn new(codec: Arc<dyn VideoCodec>) -> Self {
        Self {
            codec,
            decoders: HashMap::new(),
        }
    }

    /// How many contexts are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.decoders.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.decoders.is_empty()
    }

    /// Apply one Open H.264 rectangle (`payload` as framed by `rfb-proto`):
    /// the flags first, then the frames. Returns the picture to draw into
    /// `rect`, cropped to it, or `None` for a control message, a frame that
    /// needs more data, or one the decoder could not read (the context is
    /// dropped then and the picture stays as it was until the server starts a
    /// new one with an intra frame, as the spec recommends).
    ///
    /// # Errors
    /// [`DecodeError`] for a malformed message, a rectangle that does not fit
    /// the picture the stream decodes to, or no way to make a decoder.
    ///
    /// # Panics
    /// Never in practice: the context looked up was inserted a line before.
    pub fn apply(&mut self, rect: Rect, payload: &[u8]) -> Result<Option<Vec<u8>>, DecodeError> {
        let mut r = Reader::new(payload);
        let (len, flags) = match (r.u32(), r.u32()) {
            (Ok(len), Ok(flags)) => (len as usize, flags),
            _ => return Err(DecodeError::Malformed("truncated H.264 header")),
        };
        if len > limits::MAX_H264_DATA as usize || r.remaining() != len {
            return Err(DecodeError::Malformed("H.264 length"));
        }
        let data = &payload[8..];
        if flags & RESET_ALL_CONTEXTS != 0 {
            self.decoders.clear();
        }
        if flags & RESET_CONTEXT != 0 {
            self.decoders.remove(&key(rect));
        }
        if data.is_empty() {
            return Ok(None);
        }
        if !self.decoders.contains_key(&key(rect)) {
            if self.decoders.len() >= MAX_CONTEXTS {
                return Err(DecodeError::Malformed("too many H.264 contexts"));
            }
            let decoder = self
                .codec
                .decoder()
                .map_err(|_| DecodeError::UnsupportedEncoding(rfb_proto::consts::enc::OPEN_H264))?;
            self.decoders.insert(key(rect), decoder);
        }
        let decoder = self
            .decoders
            .get_mut(&key(rect))
            .expect("inserted just above");
        let picture = match decoder.decode(data) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(None),
            Err(_) => {
                self.decoders.remove(&key(rect));
                return Ok(None);
            }
        };
        if picture.width < rect.width || picture.height < rect.height {
            return Err(DecodeError::Malformed(
                "H.264 picture smaller than its rectangle",
            ));
        }
        // Frames may be larger than the rectangle: trim the right and the bottom.
        let (pw, w, h) = (
            usize::from(picture.width) * 4,
            usize::from(rect.width) * 4,
            usize::from(rect.height),
        );
        let mut out = Vec::with_capacity(w * h);
        for row in picture.bgrx.chunks_exact(pw).take(h) {
            out.extend_from_slice(&row[..w]);
        }
        if out.len() != w * h {
            return Err(DecodeError::Malformed("H.264 picture size"));
        }
        Ok(Some(out))
    }
}

/// Build the payload of an Open H.264 rectangle.
#[must_use]
pub fn payload(flags: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{Damage, Decoder, Framebuffer};
    use rfb_proto::{PixelFormat, consts::enc, msg::server::RectHeader};

    /// "H.264" for tests: the data is `width, height` (u16 each) and then
    /// that many `B, G, R, X` pixels. `b"bad"` is undecodable and an empty
    /// picture means "needs more data".
    #[derive(Debug)]
    struct Fake;

    impl VideoDecoder for Fake {
        fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
            if data == b"bad" {
                return Err("bad data".into());
            }
            if data.len() < 4 {
                return Ok(None);
            }
            let (width, height) = (
                u16::from_be_bytes([data[0], data[1]]),
                u16::from_be_bytes([data[2], data[3]]),
            );
            Ok(Some(Picture {
                width,
                height,
                bgrx: data[4..].to_vec(),
            }))
        }
    }

    #[derive(Debug, Default)]
    struct FakeCodec {
        made: Mutex<usize>,
    }

    impl VideoCodec for FakeCodec {
        fn decoder(&self) -> Result<Box<dyn VideoDecoder>, String> {
            *self.made.lock().unwrap() += 1;
            Ok(Box::new(Fake))
        }

        fn encoder(&self, _: u16, _: u16) -> Result<Box<dyn VideoEncoder>, String> {
            Err("no encoder".into())
        }
    }

    fn frame(width: u16, height: u16, colour: [u8; 3]) -> Vec<u8> {
        let mut data = width.to_be_bytes().to_vec();
        data.extend_from_slice(&height.to_be_bytes());
        for _ in 0..usize::from(width) * usize::from(height) {
            data.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
        data
    }

    fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn frames_are_cropped_flags_reset_contexts_and_control_messages_draw_nothing() {
        let codec = Arc::new(FakeCodec::default());
        let mut c = Contexts::new(codec.clone());
        // A frame larger than the rectangle is trimmed at the right and bottom.
        let out = c
            .apply(rect(0, 0, 2, 1), &payload(0, &frame(3, 2, [1, 2, 3])))
            .unwrap()
            .unwrap();
        assert_eq!(out, [1, 2, 3, 255, 1, 2, 3, 255]);
        assert_eq!(c.len(), 1);
        // The same rectangle keeps its context; another rectangle gets its own.
        c.apply(rect(0, 0, 2, 1), &payload(0, &frame(2, 1, [4, 5, 6])))
            .unwrap();
        c.apply(rect(10, 0, 2, 1), &payload(0, &frame(2, 1, [7, 8, 9])))
            .unwrap();
        assert_eq!((c.len(), *codec.made.lock().unwrap()), (2, 2));
        // ResetContext replaces only this rectangle's context; a control message
        // (no data) draws nothing.
        assert_eq!(
            c.apply(rect(0, 0, 2, 1), &payload(RESET_CONTEXT, &[]))
                .unwrap(),
            None
        );
        assert_eq!(c.len(), 1);
        assert_eq!(
            c.apply(rect(0, 0, 2, 1), &payload(RESET_ALL_CONTEXTS, &[]))
                .unwrap(),
            None
        );
        assert!(c.is_empty());
        // Data that needs more does not draw.
        assert_eq!(
            c.apply(rect(0, 0, 2, 1), &payload(0, &[0, 2])).unwrap(),
            None
        );
    }

    #[test]
    fn a_bad_frame_drops_its_context_and_a_broken_message_is_an_error() {
        let mut c = Contexts::new(Arc::new(FakeCodec::default()));
        c.apply(rect(0, 0, 1, 1), &payload(0, &frame(1, 1, [0, 0, 0])))
            .unwrap();
        assert_eq!(
            c.apply(rect(0, 0, 1, 1), &payload(0, b"bad")).unwrap(),
            None
        );
        assert!(c.is_empty(), "waits for the server to start again");
        // Too small a picture, a wrong length and a short header are errors.
        assert!(
            c.apply(rect(0, 0, 4, 4), &payload(0, &frame(2, 2, [0; 3])))
                .is_err()
        );
        let mut wrong = payload(0, &frame(1, 1, [0; 3]));
        wrong[3] += 1;
        assert!(c.apply(rect(0, 0, 1, 1), &wrong).is_err());
        assert!(c.apply(rect(0, 0, 1, 1), &[0, 0, 0]).is_err());
    }

    #[test]
    fn no_more_than_64_contexts() {
        let mut c = Contexts::new(Arc::new(FakeCodec::default()));
        for i in 0..u16::try_from(MAX_CONTEXTS).unwrap() {
            c.apply(rect(i, 0, 1, 1), &payload(0, &frame(1, 1, [0; 3])))
                .unwrap();
        }
        assert!(
            c.apply(rect(100, 0, 1, 1), &payload(0, &frame(1, 1, [0; 3])))
                .is_err()
        );
        // Resetting makes room again.
        c.apply(
            rect(100, 0, 1, 1),
            &payload(RESET_ALL_CONTEXTS, &frame(1, 1, [0; 3])),
        )
        .unwrap();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn the_decoder_draws_h264_rectangles_into_the_framebuffer() {
        let mut fb = Framebuffer::new(8, 4).unwrap();
        let mut d = Decoder::new(&PixelFormat::RGB888).unwrap();
        let header = RectHeader {
            rect: rect(2, 1, 2, 2),
            encoding: enc::OPEN_H264,
        };
        let data = payload(RESET_CONTEXT, &frame(2, 2, [10, 20, 30]));
        // Without a codec the encoding is unsupported.
        assert!(d.decode(&mut fb, &header, &data).is_err());
        d.set_video(Arc::new(FakeCodec::default()));
        assert_eq!(
            d.decode(&mut fb, &header, &data).unwrap(),
            Damage::Rect(header.rect)
        );
        let at = |x: usize, y: usize| &fb.pixels()[(y * 8 + x) * 4..][..4];
        assert_eq!(at(2, 1), [10, 20, 30, 255]);
        assert_eq!(at(3, 2), [10, 20, 30, 255]);
        assert_eq!(at(0, 0), [0, 0, 0, 255]);
        assert_eq!(at(4, 1), [0, 0, 0, 255]);
    }
}
