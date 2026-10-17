//! The tiles shared by ZRLE (64 x 64, inside zlib) and TRLE (16 x 16, plain):
//! raw, solid, packed palette, plain RLE and palette RLE, and for TRLE the
//! reuse of the previous tile's palette (RFC 6143 section 7.7.5).

use rfb_proto::{types::Rect, wire::Reader};

use crate::{DecodeError, Framebuffer, PixelConverter};

/// The palette of the last tile that had one (2 to 127 colours), which
/// subencodings 127 and 129 reuse. ZRLE does not allow reuse.
#[derive(Debug)]
pub(crate) struct Palettes {
    allow_reuse: bool,
    last: Vec<[u8; 4]>,
}

impl Palettes {
    pub(crate) fn new(allow_reuse: bool) -> Self {
        Self {
            allow_reuse,
            last: Vec::new(),
        }
    }
}

/// Decode `data`, the tiles of `rect`, into `fb`. All of `data` must be used.
pub(crate) fn decode_tiles(
    fb: &mut Framebuffer,
    conv: &PixelConverter,
    rect: Rect,
    data: &[u8],
    tile_size: u16,
    allow_reuse: bool,
) -> Result<(), DecodeError> {
    let cpix = conv.cpixel_len();
    let mut r = Reader::new(data);
    let mut palettes = Palettes::new(allow_reuse);
    let mut tile_pixels: Vec<[u8; 4]> =
        Vec::with_capacity(usize::from(tile_size) * usize::from(tile_size));
    for ty in (0..rect.height).step_by(usize::from(tile_size)) {
        let th = (rect.height - ty).min(tile_size);
        for tx in (0..rect.width).step_by(usize::from(tile_size)) {
            let tw = (rect.width - tx).min(tile_size);
            decode_tile(
                &mut r,
                conv,
                cpix,
                (tw, th),
                &mut tile_pixels,
                &mut palettes,
            )?;
            let tile = Rect {
                x: rect.x + tx,
                y: rect.y + ty,
                width: tw,
                height: th,
            };
            for (row, src) in (0..th).zip(tile_pixels.chunks_exact(usize::from(tw))) {
                for (dst, px) in fb
                    .row_mut(tile, row)
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src)
                {
                    dst.copy_from_slice(px);
                }
            }
        }
    }
    if r.remaining() != 0 {
        return Err(DecodeError::Malformed("trailing bytes after the tiles"));
    }
    Ok(())
}

/// Run length: one plus the sum of bytes, ending at the first byte below 255.
fn run_length(r: &mut Reader<'_>) -> Result<usize, DecodeError> {
    let mut len = 1usize;
    loop {
        let b = r
            .u8()
            .map_err(|_| DecodeError::Malformed("truncated run length"))?;
        len += usize::from(b);
        if b != 255 {
            return Ok(len);
        }
    }
}

fn push_run(
    pixels: &mut Vec<[u8; 4]>,
    px: [u8; 4],
    run: usize,
    count: usize,
) -> Result<(), DecodeError> {
    if pixels.len() + run > count {
        return Err(DecodeError::Malformed("run past the end of the tile"));
    }
    pixels.resize(pixels.len() + run, px);
    Ok(())
}

/// Decode one tile's pixels, row by row, into `tile_pixels`.
fn decode_tile(
    r: &mut Reader<'_>,
    conv: &PixelConverter,
    cpix: usize,
    (tw, th): (u16, u16),
    tile_pixels: &mut Vec<[u8; 4]>,
    palettes: &mut Palettes,
) -> Result<(), DecodeError> {
    let short = |_| DecodeError::Malformed("truncated tile data");
    let count = usize::from(tw) * usize::from(th);
    tile_pixels.clear();
    let sub = r.u8().map_err(short)?;
    let (rle, palette_size) = (sub & 0x80 != 0, usize::from(sub & 0x7F));
    let read_cpixel = |r: &mut Reader<'_>| -> Result<[u8; 4], DecodeError> {
        Ok(conv.cpixel(r.take(cpix).map_err(short)?))
    };
    let read_palette = |r: &mut Reader<'_>, n: usize| -> Result<Vec<[u8; 4]>, DecodeError> {
        (0..n).map(|_| read_cpixel(r)).collect()
    };
    // Subencodings 127 and 129 mean "the previous palette".
    let reused = match (rle, palette_size) {
        (false, 127) | (true, 1) => {
            if !palettes.allow_reuse {
                return Err(DecodeError::Malformed("palette reuse is not allowed here"));
            }
            if palettes.last.is_empty() {
                return Err(DecodeError::Malformed("palette reused before any"));
            }
            true
        }
        _ => false,
    };
    match (rle, palette_size) {
        (false, 0) => {
            for _ in 0..count {
                tile_pixels.push(read_cpixel(r)?);
            }
        }
        (false, 1) => {
            let px = read_cpixel(r)?;
            tile_pixels.resize(count, px);
        }
        (false, 2..=16 | 127) => {
            if !reused {
                palettes.last = read_palette(r, palette_size)?;
            }
            let palette = &palettes.last;
            if !(2..=16).contains(&palette.len()) {
                return Err(DecodeError::Malformed(
                    "packed palette reuse of a larger palette",
                ));
            }
            let bits = match palette.len() {
                2 => 1,
                3..=4 => 2,
                _ => 4,
            };
            let row_bytes = (usize::from(tw) * bits).div_ceil(8);
            for _ in 0..th {
                let row = r.take(row_bytes).map_err(short)?;
                for x in 0..usize::from(tw) {
                    let shift = 8 - bits - (x * bits) % 8;
                    let index = usize::from(row[x * bits / 8] >> shift) & ((1 << bits) - 1);
                    tile_pixels.push(
                        *palette
                            .get(index)
                            .ok_or(DecodeError::Malformed("palette index"))?,
                    );
                }
            }
        }
        (true, 0) => {
            while tile_pixels.len() < count {
                let px = read_cpixel(r)?;
                push_run(tile_pixels, px, run_length(r)?, count)?;
            }
        }
        (true, 1..=127) => {
            if !reused {
                palettes.last = read_palette(r, palette_size)?;
            }
            let palette = &palettes.last;
            while tile_pixels.len() < count {
                let b = r.u8().map_err(short)?;
                let px = *palette
                    .get(usize::from(b & 0x7F))
                    .ok_or(DecodeError::Malformed("palette index"))?;
                let run = if b & 0x80 != 0 { run_length(r)? } else { 1 };
                push_run(tile_pixels, px, run, count)?;
            }
        }
        _ => return Err(DecodeError::Malformed("tile subencoding")),
    }
    Ok(())
}
