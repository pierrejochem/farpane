use rfb_proto::{
    PixelFormat,
    consts::{enc, pseudo},
    msg::server::RectHeader,
    types::{Rect, Screen},
    wire::Reader,
};

use crate::{DecodeError, Framebuffer, PixelConverter, inflate::Inflater};

/// A mouse cursor shape sent by the server (`Cursor` pseudo-encoding), so the
/// viewer can draw the pointer locally without waiting for a round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The pixel that sits on the pointer position.
    pub hotspot: (u16, u16),
    pub width: u16,
    pub height: u16,
    /// `R, G, B, A` rows, `A` being 0 (transparent) or 255. Empty when the
    /// cursor is hidden (width or height 0).
    pub rgba: Vec<u8>,
}

/// A screen layout from an `ExtendedDesktopSize` rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLayout {
    /// 0 changed on the server, 1 requested by us, 2 requested by another
    /// client; unknown values should be treated as 0.
    pub reason: u16,
    /// 0 for success. For a request of ours (`reason` 1) anything else is a
    /// refusal: 1 prohibited, 2 out of resources, 3 invalid layout,
    /// 4 forwarded (may complete later).
    pub status: u16,
    pub width: u16,
    pub height: u16,
    pub screens: Vec<Screen>,
    /// The framebuffer changed size and was cleared (never for a refusal, and
    /// not when the size is unchanged: servers repeat the layout on every
    /// full refresh).
    pub resized: bool,
}

/// What a decoded rectangle changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    /// Nothing to repaint (for example a pseudo-rectangle for the UI layer).
    None,
    /// This area of the framebuffer changed.
    Rect(Rect),
    /// The framebuffer was resized and cleared; repaint everything.
    Resized,
    /// The cursor shape changed. The picture is untouched.
    Cursor(Cursor),
    /// The screen layout was announced; see [`DesktopLayout::resized`].
    Layout(DesktopLayout),
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
    tight: [Inflater; 4],
}

impl Decoder {
    /// # Errors
    /// [`DecodeError::UnsupportedFormat`] if `pf` cannot be converted.
    pub fn new(pf: &PixelFormat) -> Result<Self, DecodeError> {
        Ok(Self {
            converter: PixelConverter::new(pf)?,
            zlib: Inflater::default(),
            zrle: Inflater::default(),
            tight: Default::default(),
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
            enc::TIGHT => {
                crate::tight::decode(fb, &self.converter, rect, payload, &mut self.tight)?;
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
            pseudo::CURSOR => Ok(Damage::Cursor(self.cursor(header.rect, payload)?)),
            pseudo::EXT_DESKTOP_SIZE => Ok(Damage::Layout(layout(fb, header.rect, payload)?)),
            pseudo::LAST_RECT => Ok(Damage::None),
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

    /// `Cursor` pseudo-encoding: the rectangle's `x`, `y` are the hotspot and
    /// `width` x `height` the cursor size. The payload is the pixels in the
    /// session format, then a 1-bit-per-pixel mask in rows padded to whole
    /// bytes, most significant bit first; a set bit means "opaque".
    fn cursor(&self, rect: Rect, payload: &[u8]) -> Result<Cursor, DecodeError> {
        let (w, h) = (usize::from(rect.width), usize::from(rect.height));
        let pixel_bytes = w * h * self.converter.bytes_per_pixel();
        let mask_row = w.div_ceil(8);
        if payload.len() != pixel_bytes + mask_row * h {
            return Err(DecodeError::Malformed("Cursor payload size"));
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        if w > 0 && h > 0 {
            let (pixels, mask) = payload.split_at(pixel_bytes);
            let mut bgrx = vec![0u8; w * 4];
            for (row, src) in pixels
                .chunks_exact(w * self.converter.bytes_per_pixel())
                .enumerate()
            {
                self.converter.convert_row(src, &mut bgrx);
                for (x, px) in bgrx.chunks_exact(4).enumerate() {
                    let opaque = mask[row * mask_row + x / 8] >> (7 - x % 8) & 1 == 1;
                    rgba.extend_from_slice(&[px[2], px[1], px[0], if opaque { 255 } else { 0 }]);
                }
            }
        }
        Ok(Cursor {
            hotspot: (rect.x, rect.y),
            width: rect.width,
            height: rect.height,
            rgba,
        })
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

/// `ExtendedDesktopSize`: the rectangle's `x` is the reason, `y` the status and
/// `width` x `height` the framebuffer size; the payload lists the screens.
fn layout(fb: &mut Framebuffer, rect: Rect, payload: &[u8]) -> Result<DesktopLayout, DecodeError> {
    let malformed = |_| DecodeError::Malformed("ExtendedDesktopSize payload");
    let mut r = Reader::new(payload);
    let count = r.u8().map_err(malformed)?;
    r.skip(3).map_err(malformed)?;
    let mut screens = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        screens.push(Screen::read(&mut r).map_err(malformed)?);
    }
    if r.remaining() != 0 {
        return Err(DecodeError::Malformed("ExtendedDesktopSize payload size"));
    }
    let (reason, status) = (rect.x, rect.y);
    let (width, height) = (rect.width, rect.height);
    let mut resized = false;
    if status == 0 && (width, height) != (fb.width(), fb.height()) {
        fb.resize(width, height)?;
        resized = true;
    }
    Ok(DesktopLayout {
        reason,
        status,
        width,
        height,
        screens,
        resized,
    })
}
