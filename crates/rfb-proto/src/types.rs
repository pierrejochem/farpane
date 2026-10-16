//! Core wire structures.

use crate::{ProtoError, wire::Reader};

/// The 16-byte `PIXEL_FORMAT` structure from `ServerInit` and `SetPixelFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::struct_excessive_bools)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// Length on the wire, including 3 bytes of padding.
    pub const LEN: usize = 16;

    /// 32 bits per pixel, depth 24, little-endian BGRX: the most common
    /// server format and our internal framebuffer layout.
    pub const RGB888: Self = Self {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: false,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };

    /// Bytes per pixel on the wire.
    #[must_use]
    pub const fn bytes_per_pixel(&self) -> usize {
        self.bits_per_pixel as usize / 8
    }

    /// Parse and validate.
    ///
    /// # Errors
    /// [`ProtoError::NeedMore`] on short input, [`ProtoError::Malformed`] if
    /// bits-per-pixel is not 8, 16 or 32, or depth exceeds bits-per-pixel.
    pub fn read(r: &mut Reader<'_>) -> Result<Self, ProtoError> {
        let b = r.array::<{ Self::LEN }>()?;
        let pf = Self {
            bits_per_pixel: b[0],
            depth: b[1],
            big_endian: b[2] != 0,
            true_colour: b[3] != 0,
            red_max: u16::from_be_bytes([b[4], b[5]]),
            green_max: u16::from_be_bytes([b[6], b[7]]),
            blue_max: u16::from_be_bytes([b[8], b[9]]),
            red_shift: b[10],
            green_shift: b[11],
            blue_shift: b[12],
        };
        if !matches!(pf.bits_per_pixel, 8 | 16 | 32) {
            return Err(ProtoError::Malformed {
                ctx: "PixelFormat bits-per-pixel",
            });
        }
        if pf.depth > pf.bits_per_pixel {
            return Err(ProtoError::Malformed {
                ctx: "PixelFormat depth",
            });
        }
        Ok(pf)
    }

    /// Serialise to the 16-byte wire form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0] = self.bits_per_pixel;
        b[1] = self.depth;
        b[2] = u8::from(self.big_endian);
        b[3] = u8::from(self.true_colour);
        b[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        b[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        b[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        b[10] = self.red_shift;
        b[11] = self.green_shift;
        b[12] = self.blue_shift;
        b
    }
}

/// A rectangle in framebuffer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    /// True if the rectangle lies completely inside a `width` × `height`
    /// framebuffer. Pseudo-rectangles are exempt from this check.
    #[must_use]
    pub fn fits(&self, width: u16, height: u16) -> bool {
        u32::from(self.x) + u32::from(self.width) <= u32::from(width)
            && u32::from(self.y) + u32::from(self.height) <= u32::from(height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_round_trips() {
        let bytes = PixelFormat::RGB888.to_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(PixelFormat::read(&mut r), Ok(PixelFormat::RGB888));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn pixel_format_rejects_bad_bpp() {
        let mut bytes = PixelFormat::RGB888.to_bytes();
        bytes[0] = 24;
        assert!(PixelFormat::read(&mut Reader::new(&bytes)).is_err());
    }

    #[test]
    fn rect_bounds_do_not_overflow() {
        let r = Rect {
            x: u16::MAX,
            y: 0,
            width: 2,
            height: 1,
        };
        assert!(!r.fits(u16::MAX, 10));
    }
}
