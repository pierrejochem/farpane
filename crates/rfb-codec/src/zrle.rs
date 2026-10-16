//! ZRLE: zlib-compressed 64x64 tiles with palettes and run-length coding.

use rfb_proto::{types::Rect, wire::Reader};

use crate::{DecodeError, Framebuffer, PixelConverter, inflate::Inflater};

const TILE: u16 = 64;

pub(crate) fn decode(
    fb: &mut Framebuffer,
    conv: &PixelConverter,
    rect: Rect,
    payload: &[u8],
    inflater: &mut Inflater,
) -> Result<(), DecodeError> {
    fb.check(rect)?;
    let mut header = Reader::new(payload);
    let len = header
        .u32()
        .map_err(|_| DecodeError::Malformed("truncated ZRLE payload"))? as usize;
    if len != payload.len() - 4 {
        return Err(DecodeError::Malformed("ZRLE length prefix"));
    }
    let cpix = conv.cpixel_len();
    let (w, h) = (usize::from(rect.width), usize::from(rect.height));
    let tiles = w.div_ceil(usize::from(TILE)) * h.div_ceil(usize::from(TILE));
    // Worst case: every pixel as its own run plus a full palette per tile.
    let limit = w * h * (cpix + 1) + tiles * (2 + 128 * cpix);
    let data = inflater.inflate(&payload[4..], limit)?;

    let mut r = Reader::new(&data);
    let mut tile_pixels: Vec<[u8; 4]> = Vec::with_capacity(usize::from(TILE) * usize::from(TILE));
    for ty in (0..rect.height).step_by(usize::from(TILE)) {
        let th = (rect.height - ty).min(TILE);
        for tx in (0..rect.width).step_by(usize::from(TILE)) {
            let tw = (rect.width - tx).min(TILE);
            decode_tile(&mut r, conv, cpix, tw, th, &mut tile_pixels)?;
            let tile = Rect {
                x: rect.x + tx,
                y: rect.y + ty,
                width: tw,
                height: th,
            };
            for (row, src) in (0..th).zip(tile_pixels.chunks_exact(usize::from(tw))) {
                for (dst, px) in fb.row_mut(tile, row).chunks_exact_mut(4).zip(src) {
                    dst.copy_from_slice(px);
                }
            }
        }
    }
    if r.remaining() != 0 {
        return Err(DecodeError::Malformed("trailing bytes after ZRLE tiles"));
    }
    Ok(())
}

/// Run length: one plus the sum of bytes, ending at the first byte below 255.
fn run_length(r: &mut Reader<'_>) -> Result<usize, DecodeError> {
    let mut len = 1usize;
    loop {
        let b = r
            .u8()
            .map_err(|_| DecodeError::Malformed("truncated ZRLE run length"))?;
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
        return Err(DecodeError::Malformed("ZRLE run past the end of the tile"));
    }
    pixels.resize(pixels.len() + run, px);
    Ok(())
}

/// Decode one tile's pixels, row by row, into `tile_pixels`.
fn decode_tile(
    r: &mut Reader<'_>,
    conv: &PixelConverter,
    cpix: usize,
    tw: u16,
    th: u16,
    tile_pixels: &mut Vec<[u8; 4]>,
) -> Result<(), DecodeError> {
    let short = |_| DecodeError::Malformed("truncated ZRLE tile data");
    let count = usize::from(tw) * usize::from(th);
    tile_pixels.clear();
    let sub = r.u8().map_err(short)?;
    let (rle, palette_size) = (sub & 0x80 != 0, usize::from(sub & 0x7F));
    let read_cpixel = |r: &mut Reader<'_>| -> Result<[u8; 4], DecodeError> {
        Ok(conv.cpixel(r.take(cpix).map_err(short)?))
    };
    let mut palette = Vec::with_capacity(palette_size);
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
        (false, 2..=16) => {
            for _ in 0..palette_size {
                palette.push(read_cpixel(r)?);
            }
            let bits = match palette_size {
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
                            .ok_or(DecodeError::Malformed("ZRLE palette index"))?,
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
        (true, 2..=127) => {
            for _ in 0..palette_size {
                palette.push(read_cpixel(r)?);
            }
            while tile_pixels.len() < count {
                let b = r.u8().map_err(short)?;
                let px = *palette
                    .get(usize::from(b & 0x7F))
                    .ok_or(DecodeError::Malformed("ZRLE palette index"))?;
                let run = if b & 0x80 != 0 { run_length(r)? } else { 1 };
                push_run(tile_pixels, px, run, count)?;
            }
        }
        _ => return Err(DecodeError::Malformed("ZRLE subencoding")),
    }
    Ok(())
}
