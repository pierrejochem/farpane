//! Encoders for the server: Raw and ZRLE.
//!
//! They read the server's 32 bit framebuffer ([`Framebuffer`]) and produce
//! the payload that [`rfb_proto`]'s framers and the viewer's decoders expect.

use flate2::Compression;
use rfb_proto::types::Rect;

use crate::{Cursor, DecodeError, Framebuffer, PixelPacker, framebuffer::BYTES_PER_PIXEL};

/// Raw: the pixels row by row in the wire format.
///
/// # Errors
/// [`DecodeError::OutOfBounds`] if `rect` is not inside the picture.
pub fn encode_raw(
    fb: &Framebuffer,
    rect: Rect,
    packer: &PixelPacker,
) -> Result<Vec<u8>, DecodeError> {
    fb.check(rect)?;
    let mut out = Vec::with_capacity(
        usize::from(rect.width) * usize::from(rect.height) * packer.bytes_per_pixel(),
    );
    for row in 0..rect.height {
        for px in fb.row(rect, row).as_chunks::<BYTES_PER_PIXEL>().0 {
            packer.pack([px[0], px[1], px[2], px[3]], &mut out);
        }
    }
    Ok(out)
}

/// The payload of a `Cursor` pseudo-rectangle: the pixels in the wire
/// format, then a 1 bit per pixel mask (rows padded to whole bytes, most
/// significant bit first) in which a set bit means "opaque". Pixels with an
/// alpha of 128 or more count as opaque.
///
/// # Errors
/// [`DecodeError::Malformed`] if `cursor.rgba` is not `width * height` pixels.
pub fn encode_cursor(cursor: &Cursor, packer: &PixelPacker) -> Result<Vec<u8>, DecodeError> {
    let (w, h) = (usize::from(cursor.width), usize::from(cursor.height));
    if cursor.rgba.len() != w * h * 4 {
        return Err(DecodeError::Malformed("cursor pixel data size"));
    }
    let mask_row = w.div_ceil(8);
    let mut out = Vec::with_capacity(w * h * packer.bytes_per_pixel() + mask_row * h);
    for px in cursor.rgba.as_chunks::<4>().0 {
        packer.pack([px[2], px[1], px[0], 0xFF], &mut out);
    }
    let mut mask = vec![0u8; mask_row * h];
    for (i, px) in cursor.rgba.as_chunks::<4>().0.iter().enumerate() {
        if px[3] >= 128 {
            mask[(i / w) * mask_row + (i % w) / 8] |= 0x80 >> (i % w % 8);
        }
    }
    out.extend_from_slice(&mask);
    Ok(out)
}

const TILE: u16 = 64;

/// A zlib stream that ends every chunk with a sync flush, as RFB servers do,
/// so each chunk can be decoded as soon as it arrives.
///
/// This drives `flate2::Compress` by hand. Its `write::ZlibEncoder::flush`
/// can return before a large sync flush has been written out completely, and
/// the lost tail corrupts the stream (seen with 192 x 192 ZRLE rectangles of
/// gradient content). zlib's rule is to call again with the same flush mode
/// until a call leaves output space unused; that is what this does.
struct SyncDeflate(flate2::Compress);

impl SyncDeflate {
    fn new(level: u32) -> Self {
        Self(flate2::Compress::new(Compression::new(level.min(9)), true))
    }

    /// The compressed form of `input` as the next chunk of the stream.
    fn compress(&mut self, input: &[u8]) -> Vec<u8> {
        let start = self.0.total_in();
        let mut out = Vec::with_capacity(input.len() / 2 + 64);
        loop {
            let consumed = usize::try_from(self.0.total_in() - start).unwrap_or(usize::MAX);
            if out.capacity() - out.len() < 64 {
                out.reserve(out.len().max(4096));
            }
            let before = (consumed, out.len());
            // Compressing into a `Vec` with room cannot fail.
            let _ = self.0.compress_vec(
                &input[consumed.min(input.len())..],
                &mut out,
                flate2::FlushCompress::Sync,
            );
            let consumed = usize::try_from(self.0.total_in() - start).unwrap_or(usize::MAX);
            // Finished when everything went in and the call left room over:
            // a call that fills the buffer has more to write.
            if consumed >= input.len() && out.len() < out.capacity() {
                break;
            }
            if (consumed, out.len()) == before {
                // No progress at all: cannot happen with room to write.
                out.reserve(4096);
            }
        }
        out
    }
}

/// A ZRLE encoder. One per connection: the zlib stream lives as long as the
/// connection, so rectangles must be sent in the order they were encoded.
pub struct ZrleEncoder {
    z: SyncDeflate,
}

impl core::fmt::Debug for ZrleEncoder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ZrleEncoder")
    }
}

impl Default for ZrleEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZrleEncoder {
    /// An encoder at zlib's default level (6).
    #[must_use]
    pub fn new() -> Self {
        Self::with_level(Compression::default().level())
    }

    /// An encoder at zlib `level`, 0 (none) to 9 (smallest, slowest); larger
    /// values count as 9. Low levels trade size for speed, which pays on a
    /// fast link.
    #[must_use]
    pub fn with_level(level: u32) -> Self {
        Self {
            z: SyncDeflate::new(level),
        }
    }

    /// Encode `rect`: a `U32` length and the zlib data, sync-flushed.
    ///
    /// # Errors
    /// [`DecodeError::OutOfBounds`] if `rect` is not inside the picture.
    pub fn encode(
        &mut self,
        fb: &Framebuffer,
        rect: Rect,
        packer: &PixelPacker,
    ) -> Result<Vec<u8>, DecodeError> {
        let data = self.z.compress(&tiles(fb, rect, packer)?);
        let mut out = Vec::with_capacity(data.len() + 4);
        out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(&data);
        Ok(out)
    }
}

/// The widest rectangle Tight can carry (RFC 6143 does not say; the Tight
/// specification does): wider ones must be split by the caller.
pub const TIGHT_MAX_WIDTH: u16 = 2048;

/// A Tight encoder without JPEG: fill for one colour, the palette filter for
/// up to 256 colours and the copy filter otherwise, with the zlib streams
/// kept for the whole connection (stream 0 for full colour, 1 for two
/// colours, 2 for larger palettes). One per connection; rectangles must be
/// sent in the order they were encoded.
pub struct TightEncoder {
    streams: [SyncDeflate; 3],
}

impl core::fmt::Debug for TightEncoder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("TightEncoder")
    }
}

impl Default for TightEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Tight's 1 to 3 byte length.
fn compact_length(out: &mut Vec<u8>, len: usize) {
    out.push(u8::try_from(len & 0x7F).unwrap_or(0) | if len > 0x7F { 0x80 } else { 0 });
    if len > 0x7F {
        out.push(
            u8::try_from((len >> 7) & 0x7F).unwrap_or(0) | if len > 0x3FFF { 0x80 } else { 0 },
        );
        if len > 0x3FFF {
            out.push(u8::try_from(len >> 14).unwrap_or(0));
        }
    }
}

impl TightEncoder {
    /// An encoder at zlib's default level (6).
    #[must_use]
    pub fn new() -> Self {
        Self::with_level(Compression::default().level())
    }

    /// An encoder at zlib `level`; see [`ZrleEncoder::with_level`].
    #[must_use]
    pub fn with_level(level: u32) -> Self {
        Self {
            streams: [
                SyncDeflate::new(level),
                SyncDeflate::new(level),
                SyncDeflate::new(level),
            ],
        }
    }

    /// Encode `rect` (at most [`TIGHT_MAX_WIDTH`] wide).
    ///
    /// # Errors
    /// [`DecodeError::OutOfBounds`] if `rect` is not inside the picture,
    /// [`DecodeError::Malformed`] if it is too wide.
    pub fn encode(
        &mut self,
        fb: &Framebuffer,
        rect: Rect,
        packer: &PixelPacker,
    ) -> Result<Vec<u8>, DecodeError> {
        fb.check(rect)?;
        if rect.width > TIGHT_MAX_WIDTH {
            return Err(DecodeError::Malformed("Tight rectangle wider than 2048"));
        }
        let (w, h) = (usize::from(rect.width), usize::from(rect.height));
        let tp = packer.tpixel_len();
        let mut pixels: Vec<[u8; 4]> = Vec::with_capacity(w * h);
        for row in 0..rect.height {
            for px in fb.row(rect, row).as_chunks::<BYTES_PER_PIXEL>().0 {
                pixels.push([px[0], px[1], px[2], 0xFF]);
            }
        }
        // Up to 256 distinct colours, in order of first use.
        let mut palette: Vec<[u8; 4]> = Vec::new();
        for px in &pixels {
            if !palette.contains(px) {
                palette.push(*px);
                if palette.len() > 256 {
                    break;
                }
            }
        }
        let mut out = Vec::new();
        if palette.len() == 1 {
            out.push(0x80);
            packer.pack_tpixel(palette[0], &mut out);
            return Ok(out);
        }
        let n = palette.len();
        let indexed_size = if n == 2 { w.div_ceil(8) * h } else { w * h };
        if n <= 256 && n * tp + indexed_size < w * h * tp {
            // Palette filter.
            let stream = if n == 2 { 1 } else { 2 };
            out.push(0x40 | (stream << 4));
            out.push(1);
            out.push(u8::try_from(n - 1).unwrap_or(0));
            for &px in &palette {
                packer.pack_tpixel(px, &mut out);
            }
            let index = |px: &[u8; 4]| palette.iter().position(|p| p == px).unwrap_or(0);
            let mut data = Vec::with_capacity(indexed_size);
            if n == 2 {
                for row in pixels.chunks_exact(w) {
                    let mut byte = 0u8;
                    for (x, px) in row.iter().enumerate() {
                        if index(px) == 1 {
                            byte |= 0x80 >> (x % 8);
                        }
                        if x % 8 == 7 {
                            data.push(byte);
                            byte = 0;
                        }
                    }
                    if w % 8 != 0 {
                        data.push(byte);
                    }
                }
            } else {
                data.extend(pixels.iter().map(|px| u8::try_from(index(px)).unwrap_or(0)));
            }
            self.finish(&mut out, usize::from(stream), &data);
        } else {
            // Copy filter: full-colour rows.
            let mut data = Vec::with_capacity(w * h * tp);
            for &px in &pixels {
                packer.pack_tpixel(px, &mut data);
            }
            out.push(0x00);
            self.finish(&mut out, 0, &data);
        }
        Ok(out)
    }

    /// Data under 12 bytes goes as it is, anything else deflated on `stream`.
    fn finish(&mut self, out: &mut Vec<u8>, stream: usize, data: &[u8]) {
        if data.len() < 12 {
            out.extend_from_slice(data);
        } else {
            let z = self.streams[stream].compress(data);
            compact_length(out, z.len());
            out.extend_from_slice(&z);
        }
    }
}

/// Hextile subencoding mask bits (RFC 6143 section 7.7.4).
mod hextile_bits {
    pub const RAW: u8 = 1;
    pub const BACKGROUND: u8 = 2;
    pub const FOREGROUND: u8 = 4;
    pub const SUBRECTS: u8 = 8;
    pub const COLOURED: u8 = 16;
}

/// Hextile: 16 x 16 tiles, each raw or a background plus rectangles of one
/// or several colours. The background and foreground carry over from tile to
/// tile and are only sent when they change. Stateless, so a result could be
/// shared between clients with the same pixel format.
///
/// # Errors
/// [`DecodeError::OutOfBounds`] if `rect` is not inside the picture.
pub fn encode_hextile(
    fb: &Framebuffer,
    rect: Rect,
    packer: &PixelPacker,
) -> Result<Vec<u8>, DecodeError> {
    fb.check(rect)?;
    let bpp = packer.bytes_per_pixel();
    let mut out = Vec::new();
    // What the decoder currently believes; `None` after a raw tile, which
    // leaves it unspecified.
    let (mut bg, mut fg): (Option<[u8; 4]>, Option<[u8; 4]>) = (None, None);
    let mut pixels: Vec<[u8; 4]> = Vec::with_capacity(256);
    for ty in (0..rect.height).step_by(16) {
        let th = (rect.height - ty).min(16);
        for tx in (0..rect.width).step_by(16) {
            let tw = (rect.width - tx).min(16);
            let tile = Rect {
                x: rect.x + tx,
                y: rect.y + ty,
                width: tw,
                height: th,
            };
            pixels.clear();
            for row in 0..th {
                for px in fb.row(tile, row).as_chunks::<BYTES_PER_PIXEL>().0 {
                    pixels.push([px[0], px[1], px[2], 0xFF]);
                }
            }
            hextile_tile(
                &pixels,
                usize::from(tw),
                packer,
                bpp,
                &mut out,
                &mut bg,
                &mut fg,
            );
        }
    }
    Ok(out)
}

/// Rectangles covering every pixel that is not `bg`, found greedily: grow to
/// the right along a row, then downwards while whole rows match.
fn subrects(
    pixels: &[[u8; 4]],
    width: usize,
    bg: [u8; 4],
) -> Vec<(usize, usize, usize, usize, [u8; 4])> {
    let height = pixels.len() / width;
    let mut covered = vec![false; pixels.len()];
    let mut found = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let colour = pixels[y * width + x];
            if colour == bg || covered[y * width + x] {
                continue;
            }
            let mut w = 1;
            while x + w < width
                && pixels[y * width + x + w] == colour
                && !covered[y * width + x + w]
            {
                w += 1;
            }
            let mut h = 1;
            'down: while y + h < height {
                for dx in 0..w {
                    let i = (y + h) * width + x + dx;
                    if pixels[i] != colour || covered[i] {
                        break 'down;
                    }
                }
                h += 1;
            }
            for dy in 0..h {
                for dx in 0..w {
                    covered[(y + dy) * width + x + dx] = true;
                }
            }
            found.push((x, y, w, h, colour));
        }
    }
    found
}

fn hextile_tile(
    pixels: &[[u8; 4]],
    width: usize,
    packer: &PixelPacker,
    bpp: usize,
    out: &mut Vec<u8>,
    bg_before: &mut Option<[u8; 4]>,
    fg_in_use: &mut Option<[u8; 4]>,
) {
    // Colours by frequency (most frequent first), stopping once there are
    // too many for rectangles to pay off.
    let mut counts: Vec<([u8; 4], usize)> = Vec::new();
    for px in pixels {
        if let Some(entry) = counts.iter_mut().find(|(c, _)| c == px) {
            entry.1 += 1;
        } else {
            counts.push((*px, 1));
        }
    }
    counts.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    let bg = counts[0].0;
    let raw_size = 1 + pixels.len() * bpp;
    let write_raw = |out: &mut Vec<u8>| {
        out.push(hextile_bits::RAW);
        for &px in pixels {
            packer.pack(px, out);
        }
    };
    let mut mask = 0u8;
    let mut body = Vec::new();
    if *bg_before != Some(bg) {
        mask |= hextile_bits::BACKGROUND;
        packer.pack(bg, &mut body);
    }
    if counts.len() > 1 {
        let rects = subrects(pixels, width, bg);
        if rects.len() > 255 {
            write_raw(out);
            (*bg_before, *fg_in_use) = (None, None);
            return;
        }
        mask |= hextile_bits::SUBRECTS;
        let coloured = counts.len() > 2;
        let fg = counts[1].0;
        if coloured {
            mask |= hextile_bits::COLOURED;
        } else if *fg_in_use != Some(fg) {
            mask |= hextile_bits::FOREGROUND;
            packer.pack(fg, &mut body);
        }
        body.push(u8::try_from(rects.len()).unwrap_or(255));
        for (x, y, w, h, colour) in rects {
            if coloured {
                packer.pack(colour, &mut body);
            }
            // Positions and sizes are below 16: they fit a nibble.
            body.push(u8::try_from(x << 4 | y).unwrap_or(0));
            body.push(u8::try_from((w - 1) << 4 | (h - 1)).unwrap_or(0));
        }
        if !coloured {
            *fg_in_use = Some(fg);
        }
    }
    if 1 + body.len() >= raw_size {
        write_raw(out);
        (*bg_before, *fg_in_use) = (None, None);
        return;
    }
    out.push(mask);
    out.extend_from_slice(&body);
    *bg_before = Some(bg);
}

/// The uncompressed ZRLE data of `rect`: 64 x 64 tiles, each with the
/// cheapest of the sub-encodings.
pub(crate) fn tiles(
    fb: &Framebuffer,
    rect: Rect,
    packer: &PixelPacker,
) -> Result<Vec<u8>, DecodeError> {
    tiles_of(fb, rect, packer, TILE, false)
}

/// TRLE: 16 x 16 tiles without zlib, each free to reuse the palette of the
/// last tile that had one (RFC 6143 section 7.7.5).
///
/// # Errors
/// [`DecodeError::OutOfBounds`] if `rect` is not inside the picture.
pub fn encode_trle(
    fb: &Framebuffer,
    rect: Rect,
    packer: &PixelPacker,
) -> Result<Vec<u8>, DecodeError> {
    tiles_of(fb, rect, packer, 16, true)
}

fn tiles_of(
    fb: &Framebuffer,
    rect: Rect,
    packer: &PixelPacker,
    size: u16,
    allow_reuse: bool,
) -> Result<Vec<u8>, DecodeError> {
    fb.check(rect)?;
    let mut out = Vec::new();
    let mut last_palette: Vec<[u8; 4]> = Vec::new();
    let mut pixels: Vec<[u8; 4]> = Vec::with_capacity(usize::from(size) * usize::from(size));
    for ty in (0..rect.height).step_by(usize::from(size)) {
        let th = (rect.height - ty).min(size);
        for tx in (0..rect.width).step_by(usize::from(size)) {
            let tw = (rect.width - tx).min(size);
            let tile = Rect {
                x: rect.x + tx,
                y: rect.y + ty,
                width: tw,
                height: th,
            };
            pixels.clear();
            for row in 0..th {
                for px in fb.row(tile, row).as_chunks::<BYTES_PER_PIXEL>().0 {
                    // The alpha byte is not part of the colour.
                    pixels.push([px[0], px[1], px[2], 0xFF]);
                }
            }
            encode_tile(
                &pixels,
                usize::from(tw),
                packer,
                &mut out,
                allow_reuse.then_some(&mut last_palette),
            );
        }
    }
    Ok(out)
}

/// Runs of equal pixels in raster order, as (pixel, length).
fn runs(pixels: &[[u8; 4]]) -> Vec<([u8; 4], usize)> {
    let mut runs: Vec<([u8; 4], usize)> = Vec::new();
    for &px in pixels {
        match runs.last_mut() {
            Some((last, n)) if *last == px => *n += 1,
            _ => runs.push((px, 1)),
        }
    }
    runs
}

/// Bytes a run length takes: `length - 1` as 255s and a final byte below 255.
fn run_bytes(len: usize) -> usize {
    (len - 1) / 255 + 1
}

fn push_run_length(out: &mut Vec<u8>, len: usize) {
    let mut rest = len - 1;
    while rest >= 255 {
        out.push(255);
        rest -= 255;
    }
    // `rest` is below 255 here.
    out.push(u8::try_from(rest).unwrap_or(254));
}

/// Encode one tile. `reuse` is `Some` for TRLE: it holds the palette of the
/// last tile that had one, which this tile may reuse (subencodings 127 and
/// 129) and which it replaces when it brings a palette of its own.
#[allow(clippy::too_many_lines)] // one decision tree, clearer unsplit
fn encode_tile(
    pixels: &[[u8; 4]],
    width: usize,
    packer: &PixelPacker,
    out: &mut Vec<u8>,
    reuse: Option<&mut Vec<[u8; 4]>>,
) {
    let cp = packer.cpixel_len();
    // The distinct colours, in order of first use, while there are few.
    let mut palette: Vec<[u8; 4]> = Vec::new();
    for px in pixels {
        if !palette.contains(px) {
            palette.push(*px);
            if palette.len() > 127 {
                break;
            }
        }
    }
    if palette.len() == 1 {
        out.push(1);
        packer.pack_cpixel(palette[0], out);
        return;
    }
    let runs = runs(pixels);
    let rows = pixels.len() / width;
    let run_cost = |&(_, len): &(_, usize)| if len == 1 { 1 } else { 1 + run_bytes(len) };
    let packed_cost = |n: usize| rows * (width * packed_bits(n)).div_ceil(8);
    let n = palette.len();
    let mut options = vec![
        (pixels.len() * cp, Kind::Raw),
        (
            runs.iter().map(|&(_, n)| cp + run_bytes(n)).sum::<usize>(),
            Kind::PlainRle,
        ),
    ];
    if (2..=16).contains(&n) {
        options.push((n * cp + packed_cost(n), Kind::Packed));
    }
    if n <= 127 {
        options.push((
            n * cp + runs.iter().map(run_cost).sum::<usize>(),
            Kind::PaletteRle,
        ));
    }
    // The previous palette, if every colour of this tile is in it.
    let reusable = reuse
        .as_ref()
        .filter(|last| !last.is_empty() && pixels.iter().all(|px| last.contains(px)))
        .map(|last| (*last).clone());
    if let Some(last) = &reusable {
        if (2..=16).contains(&last.len()) {
            options.push((packed_cost(last.len()), Kind::PackedReuse));
        }
        options.push((
            runs.iter().map(run_cost).sum::<usize>(),
            Kind::PaletteRleReuse,
        ));
    }
    let best = options
        .into_iter()
        .min_by_key(|&(size, _)| size)
        .map_or(Kind::Raw, |(_, kind)| kind);
    // The palette the indexes refer to.
    let used: &[[u8; 4]] = match (best, &reusable) {
        (Kind::PackedReuse | Kind::PaletteRleReuse, Some(last)) => last,
        _ => &palette,
    };
    let index = |px: &[u8; 4]| used.iter().position(|p| p == px).unwrap_or(0);
    let palette_len = u8::try_from(used.len()).unwrap_or(0);
    match best {
        Kind::Raw => {
            out.push(0);
            for &px in pixels {
                packer.pack_cpixel(px, out);
            }
        }
        Kind::PlainRle => {
            out.push(128);
            for &(px, len) in &runs {
                packer.pack_cpixel(px, out);
                push_run_length(out, len);
            }
        }
        Kind::Packed | Kind::PackedReuse => {
            if best == Kind::PackedReuse {
                out.push(127);
            } else {
                out.push(palette_len);
                for &px in used {
                    packer.pack_cpixel(px, out);
                }
            }
            let bits = packed_bits(used.len());
            for row in pixels.chunks_exact(width) {
                let mut byte = 0u8;
                let mut filled = 0;
                for px in row {
                    // `index` is below 16.
                    byte = byte << bits | u8::try_from(index(px)).unwrap_or(0);
                    filled += bits;
                    if filled == 8 {
                        out.push(byte);
                        byte = 0;
                        filled = 0;
                    }
                }
                if filled > 0 {
                    out.push(byte << (8 - filled));
                }
            }
        }
        Kind::PaletteRle | Kind::PaletteRleReuse => {
            if best == Kind::PaletteRleReuse {
                out.push(129);
            } else {
                out.push(128 | palette_len);
                for &px in used {
                    packer.pack_cpixel(px, out);
                }
            }
            for &(px, len) in &runs {
                let i = u8::try_from(index(&px)).unwrap_or(0);
                if len == 1 {
                    out.push(i);
                } else {
                    out.push(i | 0x80);
                    push_run_length(out, len);
                }
            }
        }
    }
    // A tile with a palette of its own becomes the one to reuse.
    if matches!(best, Kind::Packed | Kind::PaletteRle)
        && let Some(last) = reuse
    {
        last.clone_from(&palette);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Raw,
    PlainRle,
    Packed,
    PaletteRle,
    /// TRLE subencoding 127.
    PackedReuse,
    /// TRLE subencoding 129.
    PaletteRleReuse,
}

/// Bits per pixel of a packed palette of `n` colours (2 to 16).
fn packed_bits(n: usize) -> usize {
    match n {
        2 => 1,
        3..=4 => 2,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        clippy::many_single_char_names,
        clippy::similar_names
    )]

    use rfb_proto::{PixelFormat, consts::enc, msg::server::RectHeader};

    use super::*;
    use crate::Decoder;

    fn framebuffer(w: u16, h: u16, f: impl Fn(u16, u16) -> [u8; 3]) -> Framebuffer {
        let mut wire = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(x, y);
                wire.extend_from_slice(&[b, g, r, 0]);
            }
        }
        let mut fb = Framebuffer::new(w, h).unwrap();
        let header = RectHeader {
            rect: Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            encoding: enc::RAW,
        };
        Decoder::new(&PixelFormat::RGB888)
            .unwrap()
            .decode(&mut fb, &header, &wire)
            .unwrap();
        fb
    }

    fn whole(fb: &Framebuffer) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: fb.width(),
            height: fb.height(),
        }
    }

    fn first_tile_kind(fb: &Framebuffer) -> u8 {
        let packer = PixelPacker::new(&PixelFormat::RGB888).unwrap();
        tiles(fb, whole(fb), &packer).unwrap()[0]
    }

    #[test]
    fn a_solid_tile_is_one_pixel() {
        let fb = framebuffer(64, 64, |_, _| [9, 8, 7]);
        let packer = PixelPacker::new(&PixelFormat::RGB888).unwrap();
        assert_eq!(
            tiles(&fb, whole(&fb), &packer).unwrap(),
            [1, 7, 8, 9],
            "B, G, R: the low three bytes"
        );
    }

    #[test]
    fn each_tile_picks_a_sub_encoding_that_suits_it() {
        // Two colours in stripes of one pixel: packed palette (2 colours).
        let fb = framebuffer(64, 64, |x, _| if x % 2 == 0 { [0, 0, 0] } else { [255; 3] });
        assert_eq!(first_tile_kind(&fb), 2);
        // Sixteen colours in noise: packed palette, 4 bits.
        let fb = framebuffer(64, 64, |x, y| {
            let c = ((x * 7 + y * 13) % 16) as u8 * 16;
            [c, c, 0]
        });
        assert_eq!(first_tile_kind(&fb), 16);
        // Five colours in runs of ten: palette RLE (bit 7 and a palette size).
        let fb = framebuffer(64, 64, |x, y| {
            let c = ((x / 10 + y) % 5) as u8 * 50;
            [c, 9, 9]
        });
        assert_eq!(first_tile_kind(&fb), 128 | 5);
        // Four wide bands: plain RLE beats a palette here.
        let fb = framebuffer(64, 64, |_, y| [(y / 16) as u8 * 40, 40, 200]);
        assert_eq!(first_tile_kind(&fb), 128);
        // Photo-like noise with many colours and no runs: raw.
        let fb = framebuffer(64, 64, |x, y| {
            [
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 29) as u8,
                (x * y) as u8,
            ]
        });
        assert_eq!(first_tile_kind(&fb), 0);
        // More than 127 colours but long runs: plain RLE.
        let fb = framebuffer(64, 64, |x, y| {
            let c = (y * 64 + x) / 24;
            [c as u8, (c >> 8) as u8, 77]
        });
        assert_eq!(first_tile_kind(&fb), 128);
    }

    #[test]
    fn trle_reuses_the_previous_palette_when_that_is_shorter() {
        // Two 16 x 16 tiles of one-pixel stripes in the same two colours.
        let fb = framebuffer(32, 16, |x, _| if x % 2 == 0 { [0, 0, 0] } else { [255; 3] });
        let packer = PixelPacker::new(&PixelFormat::RGB888).unwrap();
        let out = tiles_of(&fb, whole(&fb), &packer, 16, true).unwrap();
        // First tile: 2, palette (2 x 3 bytes), 16 rows x 2 bytes. Then 127 and
        // the packed pixels alone.
        assert_eq!(out[0], 2);
        let second = 1 + 6 + 32;
        assert_eq!(out[second], 127, "{out:?}");
        assert_eq!(out.len(), second + 1 + 32);
        // ZRLE (no reuse) repeats the palette instead.
        let zrle = tiles_of(&fb, whole(&fb), &packer, 16, false).unwrap();
        assert_eq!(zrle[second], 2);
        // Three colours in runs of five, the same in both tiles.
        let fb = framebuffer(32, 16, |x, y| {
            let c = (((x % 16) / 5 + y) % 3) as u8 * 100;
            [c, c, 7]
        });
        let out = tiles_of(&fb, whole(&fb), &packer, 16, true).unwrap();
        // (Three colours pack into 2 bits per pixel, which wins here: 127.)
        assert!(out.contains(&127) || out.contains(&129), "{out:?}");
    }

    #[test]
    fn a_tile_with_a_new_colour_cannot_reuse_and_brings_its_own_palette() {
        let fb = framebuffer(32, 16, |x, _| {
            if x < 16 {
                if x % 2 == 0 { [0, 0, 0] } else { [255; 3] }
            } else if x % 2 == 0 {
                [0, 0, 0]
            } else {
                [9, 9, 9] // not in the first tile's palette
            }
        });
        let packer = PixelPacker::new(&PixelFormat::RGB888).unwrap();
        let out = tiles_of(&fb, whole(&fb), &packer, 16, true).unwrap();
        assert_eq!(out[1 + 6 + 32], 2, "{out:?}");
    }

    #[test]
    fn long_runs_use_255_continuation_bytes() {
        let mut out = Vec::new();
        for len in [1usize, 2, 255, 256, 510, 511, 4096] {
            out.clear();
            push_run_length(&mut out, len);
            assert_eq!(out.len(), run_bytes(len), "{len}");
            assert_eq!(out.iter().map(|&b| usize::from(b)).sum::<usize>() + 1, len);
            assert!(out[..out.len() - 1].iter().all(|&b| b == 255));
            assert!(out[out.len() - 1] < 255);
        }
    }

    #[test]
    fn the_rectangle_must_be_inside_the_picture() {
        let fb = framebuffer(4, 4, |_, _| [0; 3]);
        let packer = PixelPacker::new(&PixelFormat::RGB888).unwrap();
        let outside = Rect {
            x: 2,
            y: 2,
            width: 4,
            height: 4,
        };
        assert_eq!(
            encode_raw(&fb, outside, &packer),
            Err(DecodeError::OutOfBounds)
        );
        assert_eq!(
            ZrleEncoder::new().encode(&fb, outside, &packer),
            Err(DecodeError::OutOfBounds)
        );
    }
}
