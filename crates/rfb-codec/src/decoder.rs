use rfb_proto::{
    PixelFormat,
    consts::{enc, pseudo},
    msg::server::RectHeader,
    types::Rect,
    wire::Reader,
};

use crate::{DecodeError, Framebuffer, PixelConverter, inflate::Inflater};

/// What a decoded rectangle changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// Nothing to repaint (for example a pseudo-rectangle for the UI layer).
    None,
    /// This area of the framebuffer changed.
    Rect(Rect),
    /// The framebuffer was resized and cleared; repaint everything.
    Resized,
}

/// Per-connection decoding state. Create one per connection and call
/// [`Decoder::set_pixel_format`] whenever a `SetPixelFormat` is sent.
///
/// The zlib and ZRLE encodings each keep one inflate stream for the life of
/// the connection, so rectangles must be decoded strictly in the order the
/// server sent them.
#[derive(Debug)]
pub struct Decoder {
    converter: PixelConverter,
    zlib: Inflater,
    zrle: Inflater,
}

impl Decoder {
    /// # Errors
    /// [`DecodeError::UnsupportedFormat`] if `pf` cannot be converted.
    pub fn new(pf: &PixelFormat) -> Result<Self, DecodeError> {
        Ok(Self {
            converter: PixelConverter::new(pf)?,
            zlib: Inflater::default(),
            zrle: Inflater::default(),
        })
    }

    /// Switch the wire pixel format.
    ///
    /// # Errors
    /// As [`Decoder::new`]; the old format stays in force on error.
    pub fn set_pixel_format(&mut self, pf: &PixelFormat) -> Result<(), DecodeError> {
        self.converter = PixelConverter::new(pf)?;
        Ok(())
    }

    /// Decode one rectangle (`payload` as returned by `rfb-proto`'s framers)
    /// into `fb`.
    ///
    /// # Errors
    /// [`DecodeError`]; the framebuffer may be partly updated.
    pub fn decode(
        &mut self,
        fb: &mut Framebuffer,
        header: &RectHeader,
        payload: &[u8],
    ) -> Result<Damage, DecodeError> {
        let rect = header.rect;
        match header.encoding {
            enc::RAW => {
                self.raw(fb, rect, payload)?;
                Ok(Damage::Rect(rect))
            }
            enc::COPY_RECT => {
                copy_rect(fb, rect, payload)?;
                Ok(Damage::Rect(rect))
            }
            enc::RRE => {
                crate::rre::decode(fb, &self.converter, rect, payload, false)?;
                Ok(Damage::Rect(rect))
            }
            enc::CO_RRE => {
                crate::rre::decode(fb, &self.converter, rect, payload, true)?;
                Ok(Damage::Rect(rect))
            }
            enc::ZLIB => {
                self.zlib_rect(fb, rect, payload)?;
                Ok(Damage::Rect(rect))
            }
            enc::ZRLE => {
                crate::zrle::decode(fb, &self.converter, rect, payload, &mut self.zrle)?;
                Ok(Damage::Rect(rect))
            }
            enc::HEXTILE => {
                crate::hextile::decode(fb, &self.converter, rect, payload)?;
                Ok(Damage::Rect(rect))
            }
            pseudo::DESKTOP_SIZE => {
                fb.resize(rect.width, rect.height)?;
                Ok(Damage::Resized)
            }
            pseudo::LAST_RECT | pseudo::CURSOR => Ok(Damage::None),
            other => Err(DecodeError::UnsupportedEncoding(other)),
        }
    }

    /// zlib encoding: `U32` length, then zlib data that inflates to Raw pixels.
    fn zlib_rect(
        &mut self,
        fb: &mut Framebuffer,
        rect: Rect,
        payload: &[u8],
    ) -> Result<(), DecodeError> {
        fb.check(rect)?;
        let mut r = Reader::new(payload);
        let len = r
            .u32()
            .map_err(|_| DecodeError::Malformed("truncated zlib payload"))?
            as usize;
        if len != payload.len() - 4 {
            return Err(DecodeError::Malformed("zlib length prefix"));
        }
        let expected =
            usize::from(rect.width) * usize::from(rect.height) * self.converter.bytes_per_pixel();
        let pixels = self.zlib.inflate(&payload[4..], expected)?;
        self.raw(fb, rect, &pixels)
    }

    fn raw(&self, fb: &mut Framebuffer, rect: Rect, payload: &[u8]) -> Result<(), DecodeError> {
        fb.check(rect)?;
        let row_bytes = usize::from(rect.width) * self.converter.bytes_per_pixel();
        if payload.len() != row_bytes * usize::from(rect.height) {
            return Err(DecodeError::Malformed("Raw payload size"));
        }
        if row_bytes == 0 {
            return Ok(());
        }
        for (row, src) in (0..rect.height).zip(payload.chunks_exact(row_bytes)) {
            self.converter.convert_row(src, fb.row_mut(rect, row));
        }
        Ok(())
    }
}

fn copy_rect(fb: &mut Framebuffer, rect: Rect, payload: &[u8]) -> Result<(), DecodeError> {
    let mut r = Reader::new(payload);
    let (sx, sy) = match (r.u16(), r.u16()) {
        (Ok(x), Ok(y)) if r.remaining() == 0 => (x, y),
        _ => return Err(DecodeError::Malformed("CopyRect payload size")),
    };
    fb.check(rect)?;
    fb.check(Rect {
        x: sx,
        y: sy,
        ..rect
    })?;
    fb.copy_within(sx, sy, rect);
    Ok(())
}
