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
        Ok(Self {
            bytes_per_pixel: pf.bytes_per_pixel(),
            big_endian: pf.big_endian,
            channels,
            identity,
        })
    }

    /// Bytes per pixel on the wire.
    #[must_use]
    pub fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
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
