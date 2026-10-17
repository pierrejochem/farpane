use rfb_proto::{consts::limits, types::Rect};

use crate::DecodeError;

/// Bytes per framebuffer pixel: `B, G, R, 0xFF`.
pub const BYTES_PER_PIXEL: usize = 4;

/// The client-side picture: 32 bits per pixel, rows without padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

impl Framebuffer {
    /// A black framebuffer.
    ///
    /// # Errors
    /// [`DecodeError::OutOfBounds`] if a side exceeds
    /// [`limits::MAX_FB_DIMENSION`].
    pub fn new(width: u16, height: u16) -> Result<Self, DecodeError> {
        if width > limits::MAX_FB_DIMENSION || height > limits::MAX_FB_DIMENSION {
            return Err(DecodeError::OutOfBounds);
        }
        let len = usize::from(width) * usize::from(height) * BYTES_PER_PIXEL;
        let mut pixels = vec![0; len];
        for px in pixels.as_chunks_mut::<BYTES_PER_PIXEL>().0 {
            px[3] = 0xFF;
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Change the size and clear the picture (`DesktopSize` semantics: the
    /// old contents are discarded).
    ///
    /// # Errors
    /// As [`Framebuffer::new`]; the framebuffer is unchanged on error.
    pub fn resize(&mut self, width: u16, height: u16) -> Result<(), DecodeError> {
        *self = Self::new(width, height)?;
        Ok(())
    }

    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Bytes per row.
    #[must_use]
    pub fn stride(&self) -> usize {
        usize::from(self.width) * BYTES_PER_PIXEL
    }

    /// All pixels, row by row, `B, G, R, 0xFF`.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// One pixel as `[b, g, r, 0xFF]`, or `None` outside the picture.
    #[must_use]
    pub fn pixel(&self, x: u16, y: u16) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = self.offset(x, y);
        self.pixels[at..at + BYTES_PER_PIXEL].try_into().ok()
    }

    /// The pixels of `rect` as tightly packed `R, G, B, 0xFF` bytes, row by
    /// row: the layout GPU texture uploads and image libraries expect.
    ///
    /// # Errors
    /// [`DecodeError::OutOfBounds`] if `rect` is not inside the picture.
    pub fn rgba(&self, rect: Rect) -> Result<Vec<u8>, DecodeError> {
        self.check(rect)?;
        let row_bytes = usize::from(rect.width) * BYTES_PER_PIXEL;
        let mut out = Vec::with_capacity(row_bytes * usize::from(rect.height));
        for row in 0..rect.height {
            let at = self.offset(rect.x, rect.y + row);
            for px in self.pixels[at..at + row_bytes]
                .as_chunks::<BYTES_PER_PIXEL>()
                .0
            {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        Ok(out)
    }

    pub(crate) fn offset(&self, x: u16, y: u16) -> usize {
        usize::from(y) * self.stride() + usize::from(x) * BYTES_PER_PIXEL
    }

    pub(crate) fn check(&self, rect: Rect) -> Result<(), DecodeError> {
        if rect.fits(self.width, self.height) {
            Ok(())
        } else {
            Err(DecodeError::OutOfBounds)
        }
    }

    /// Copy `rect`-sized pixels from `bgrx` (rows of `B, G, R, X` bytes, no
    /// padding) into the picture; the alpha byte is set opaque. A server's
    /// capture backend uses this to deliver a frame.
    ///
    /// # Errors
    /// [`DecodeError::OutOfBounds`] if `rect` is not inside the picture,
    /// [`DecodeError::Malformed`] if `bgrx` is not exactly `rect` large.
    pub fn write_bgrx(&mut self, rect: Rect, bgrx: &[u8]) -> Result<(), DecodeError> {
        self.check(rect)?;
        let row_bytes = usize::from(rect.width) * BYTES_PER_PIXEL;
        if bgrx.len() != row_bytes * usize::from(rect.height) {
            return Err(DecodeError::Malformed("frame data size"));
        }
        for (row, src) in (0..rect.height).zip(bgrx.chunks_exact(row_bytes.max(1))) {
            for (dst, px) in self
                .row_mut(rect, row)
                .as_chunks_mut::<BYTES_PER_PIXEL>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<BYTES_PER_PIXEL>().0)
            {
                dst.copy_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
        }
        Ok(())
    }

    /// The bytes of row `row` of `rect`. `rect` must have been checked.
    pub(crate) fn row(&self, rect: Rect, row: u16) -> &[u8] {
        let at = self.offset(rect.x, rect.y + row);
        &self.pixels[at..at + usize::from(rect.width) * BYTES_PER_PIXEL]
    }

    /// The bytes of row `row` of `rect`, mutable. `rect` must have been
    /// checked with [`Framebuffer::check`].
    pub(crate) fn row_mut(&mut self, rect: Rect, row: u16) -> &mut [u8] {
        let at = self.offset(rect.x, rect.y + row);
        &mut self.pixels[at..at + usize::from(rect.width) * BYTES_PER_PIXEL]
    }

    /// Fill `rect` with one pixel. `rect` must have been checked.
    pub(crate) fn fill_rect(&mut self, rect: Rect, pixel: [u8; 4]) {
        for row in 0..rect.height {
            for dst in self.row_mut(rect, row).as_chunks_mut::<BYTES_PER_PIXEL>().0 {
                dst.copy_from_slice(&pixel);
            }
        }
    }

    /// Copy `rect`-sized pixels from `(sx, sy)` to `rect`, correct for
    /// overlapping areas. Both areas must have been checked.
    pub(crate) fn copy_within(&mut self, sx: u16, sy: u16, rect: Rect) {
        let row_bytes = usize::from(rect.width) * BYTES_PER_PIXEL;
        let rows = rect.height;
        // Copy downwards-moving areas bottom-up, so a source row is read
        // before an earlier copy overwrites it. `copy_within` handles overlap
        // inside one row.
        let mut copy_row = |row: u16| {
            let from = self.offset(sx, sy + row);
            let to = self.offset(rect.x, rect.y + row);
            self.pixels.copy_within(from..from + row_bytes, to);
        };
        if sy >= rect.y {
            (0..rows).for_each(&mut copy_row);
        } else {
            (0..rows).rev().for_each(&mut copy_row);
        }
    }
}
