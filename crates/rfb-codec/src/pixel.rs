use rfb_proto::PixelFormat;

use crate::{DecodeError, framebuffer::BYTES_PER_PIXEL};

/// Converts pixels in one wire format to framebuffer pixels (`B, G, R, 0xFF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelConverter {
    bytes_per_pixel: usize,
    big_endian: bool,
    /// (shift, max) for red, green and blue.
    channels: [(u32, u32); 3],
    /// The wire format already is the framebuffer layout.
    identity: bool,
    cpixel: CPixel,
    /// Tight `TPIXEL`s are 3 bytes (R, G, B).
    tight3: bool,
}

/// How a compressed pixel (`CPIXEL`, used by ZRLE) is cut from a wire pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CPixel {
    /// Same as the wire pixel.
    Full,
    /// The three least significant bytes.
    Low3,
    /// The three most significant bytes.
    High3,
}

impl PixelConverter {
    /// # Errors
    /// [`DecodeError::UnsupportedFormat`] for colour-mapped formats, a zero
    /// channel maximum or a shift that does not fit the pixel.
    pub fn new(pf: &PixelFormat) -> Result<Self, DecodeError> {
        if !pf.true_colour {
            return Err(DecodeError::UnsupportedFormat("colour-mapped"));
        }
        let channels = [
            (u32::from(pf.red_shift), u32::from(pf.red_max)),
            (u32::from(pf.green_shift), u32::from(pf.green_max)),
            (u32::from(pf.blue_shift), u32::from(pf.blue_max)),
        ];
        let bits = u32::from(pf.bits_per_pixel);
        if channels
            .iter()
            .any(|&(shift, max)| max == 0 || shift >= bits)
        {
            return Err(DecodeError::UnsupportedFormat("channel shift or maximum"));
        }
        let identity = pf.bits_per_pixel == 32
            && !pf.big_endian
            && channels == [(16, 255), (8, 255), (0, 255)];
        // ZRLE spec: 3-byte CPIXELs for 32 bpp, depth <= 24, when all colour
        // bits fit the low or the high three bytes (low wins when both do).
        let cpixel = if pf.bits_per_pixel == 32 && pf.depth <= 24 {
            let fits_low = channels
                .iter()
                .all(|&(shift, max)| shift + (32 - max.leading_zeros()) <= 24);
            let fits_high = channels.iter().all(|&(shift, _)| shift >= 8);
            if fits_low {
                CPixel::Low3
            } else if fits_high {
                CPixel::High3
            } else {
                CPixel::Full
            }
        } else {
            CPixel::Full
        };
        Ok(Self {
            bytes_per_pixel: pf.bytes_per_pixel(),
            big_endian: pf.big_endian,
            channels,
            identity,
            cpixel,
            tight3: pf.tight_pixel_len() == 3,
        })
    }

    /// Bytes per pixel on the wire.
    #[must_use]
    pub fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
    }

    /// Bytes per compressed pixel (`CPIXEL`) in ZRLE.
    #[must_use]
    pub fn cpixel_len(&self) -> usize {
        match self.cpixel {
            CPixel::Full => self.bytes_per_pixel,
            CPixel::Low3 | CPixel::High3 => 3,
        }
    }

    /// Bytes per Tight `TPIXEL`.
    #[must_use]
    pub fn tpixel_len(&self) -> usize {
        if self.tight3 { 3 } else { self.bytes_per_pixel }
    }

    /// Convert one Tight `TPIXEL` (`src` holds at least
    /// [`PixelConverter::tpixel_len`] bytes).
    #[must_use]
    pub fn tpixel(&self, src: &[u8]) -> [u8; 4] {
        if self.tight3 {
            [src[2], src[1], src[0], 0xFF]
        } else {
            self.pixel(src)
        }
    }

    /// The three colour components of a `TPIXEL` as the wire holds them.
    pub(crate) fn tpixel_channels(&self, src: &[u8]) -> [u32; 3] {
        if self.tight3 {
            [src[0], src[1], src[2]].map(u32::from)
        } else {
            let raw = self.raw(src);
            self.channels.map(|(shift, max)| (raw >> shift) & max)
        }
    }

    /// Largest value of each component of a `TPIXEL`.
    pub(crate) fn tpixel_max(&self) -> [u32; 3] {
        if self.tight3 {
            [255; 3]
        } else {
            self.channels.map(|(_, max)| max)
        }
    }

    /// Framebuffer pixel from component values (as in
    /// [`PixelConverter::tpixel_channels`]).
    pub(crate) fn pixel_from_channels(&self, c: [u32; 3]) -> [u8; 4] {
        let [mr, mg, mb] = self.tpixel_max();
        [scale(c[2], mb), scale(c[1], mg), scale(c[0], mr), 0xFF]
    }

    /// Convert one `CPIXEL`. `src` must hold at least
    /// [`PixelConverter::cpixel_len`] bytes.
    #[must_use]
    pub fn cpixel(&self, src: &[u8]) -> [u8; 4] {
        let s = |i: usize| src[i];
        // Rebuild the full 4-byte wire pixel: the three bytes sit at the
        // low or high end, in the format's byte order.
        let full = match (self.cpixel, self.big_endian) {
            (CPixel::Full, _) => return self.pixel(src),
            (CPixel::Low3, false) | (CPixel::High3, true) => [s(0), s(1), s(2), 0],
            (CPixel::Low3, true) | (CPixel::High3, false) => [0, s(0), s(1), s(2)],
        };
        self.pixel(&full)
    }

    /// Convert one wire pixel. `src` must hold at least
    /// [`PixelConverter::bytes_per_pixel`] bytes.
    #[must_use]
    pub fn pixel(&self, src: &[u8]) -> [u8; 4] {
        let raw = self.raw(src);
        let [r, g, b] = self
            .channels
            .map(|(shift, max)| scale((raw >> shift) & max, max));
        [b, g, r, 0xFF]
    }

    /// Convert `src` (whole wire pixels) into `dst` (`4 *` as many bytes).
    ///
    /// # Panics
    /// If the lengths do not match; callers check payload sizes first.
    pub fn convert_row(&self, src: &[u8], dst: &mut [u8]) {
        assert_eq!(
            src.len() / self.bytes_per_pixel * BYTES_PER_PIXEL,
            dst.len()
        );
        if self.identity {
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d.copy_from_slice(&[s[0], s[1], s[2], 0xFF]);
            }
        } else {
            for (s, d) in src
                .chunks_exact(self.bytes_per_pixel)
                .zip(dst.chunks_exact_mut(BYTES_PER_PIXEL))
            {
                d.copy_from_slice(&self.pixel(s));
            }
        }
    }

    fn raw(&self, src: &[u8]) -> u32 {
        let mut v = 0u32;
        if self.big_endian {
            for &b in &src[..self.bytes_per_pixel] {
                v = v << 8 | u32::from(b);
            }
        } else {
            for &b in src[..self.bytes_per_pixel].iter().rev() {
                v = v << 8 | u32::from(b);
            }
        }
        v
    }
}

/// Converts framebuffer pixels (`B, G, R, 0xFF`) to one wire pixel format: the
/// reverse of [`PixelConverter`], used by encoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelPacker {
    conv: PixelConverter,
    /// (shift, max) for red, green and blue.
    channels: [(u32, u32); 3],
}

impl PixelPacker {
    /// # Errors
    /// As [`PixelConverter::new`].
    pub fn new(pf: &PixelFormat) -> Result<Self, DecodeError> {
        let conv = PixelConverter::new(pf)?;
        Ok(Self {
            conv,
            channels: conv.channels,
        })
    }

    /// Bytes per wire pixel.
    #[must_use]
    pub fn bytes_per_pixel(&self) -> usize {
        self.conv.bytes_per_pixel
    }

    /// Bytes per `CPIXEL` (ZRLE).
    #[must_use]
    pub fn cpixel_len(&self) -> usize {
        self.conv.cpixel_len()
    }

    fn wire(&self, bgrx: [u8; 4]) -> [u8; 4] {
        let [r, g, b] = [bgrx[2], bgrx[1], bgrx[0]];
        let mut raw = 0u32;
        for (value, (shift, max)) in [r, g, b].into_iter().zip(self.channels) {
            raw |= ((u32::from(value) * max + 127) / 255) << shift;
        }
        if self.conv.big_endian {
            raw.to_be_bytes()
        } else {
            raw.to_le_bytes()
        }
    }

    /// Append one wire pixel.
    pub fn pack(&self, bgrx: [u8; 4], out: &mut Vec<u8>) {
        let full = self.wire(bgrx);
        let n = self.conv.bytes_per_pixel;
        // A 16 or 8 bit pixel was built in a `u32`: keep the bytes that count.
        let bytes = if self.conv.big_endian {
            &full[4 - n..]
        } else {
            &full[..n]
        };
        out.extend_from_slice(bytes);
    }

    /// Append one `CPIXEL`: three bytes when the format allows, otherwise a
    /// full wire pixel.
    pub fn pack_cpixel(&self, bgrx: [u8; 4], out: &mut Vec<u8>) {
        match self.conv.cpixel {
            CPixel::Full => self.pack(bgrx, out),
            mode => {
                let full = self.wire(bgrx);
                // Little-endian: the low bytes come first. Big-endian: last.
                let first_three = matches!(
                    (mode, self.conv.big_endian),
                    (CPixel::Low3, false) | (CPixel::High3, true)
                );
                out.extend_from_slice(if first_three { &full[..3] } else { &full[1..] });
            }
        }
    }
}

/// Scale `value` in `0..=max` to `0..=255`, rounding to nearest.
fn scale(value: u32, max: u32) -> u8 {
    // `value <= max`, so the quotient is at most 255.
    u8::try_from((value * 255 + max / 2) / max).unwrap_or(u8::MAX)
}
