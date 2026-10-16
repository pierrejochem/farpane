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

/// Scale `value` in `0..=max` to `0..=255`, rounding to nearest.
fn scale(value: u32, max: u32) -> u8 {
    // `value <= max`, so the quotient is at most 255.
    u8::try_from((value * 255 + max / 2) / max).unwrap_or(u8::MAX)
}
