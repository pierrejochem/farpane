//! Tight: fill, basic (copy, palette or gradient filter, optionally zlib).
//!
//! JPEG rectangles are refused: they are only allowed after a client asked
//! for a JPEG quality level, which Farpane does not do.

use rfb_proto::{types::Rect, wire::Reader};

use crate::{DecodeError, Framebuffer, PixelConverter, inflate::Inflater};

/// Data shorter than this is sent without zlib.
const MIN_COMPRESSED: usize = 12;

pub(crate) fn decode(
    fb: &mut Framebuffer,
    conv: &PixelConverter,
    rect: Rect,
    payload: &[u8],
    streams: &mut [Inflater; 4],
) -> Result<(), DecodeError> {
    fb.check(rect)?;
    let short = |_| DecodeError::Malformed("truncated Tight payload");
    let mut r = Reader::new(payload);
    let control = r.u8().map_err(short)?;
    // Streams are reset even for fill and JPEG rectangles.
    for (bit, stream) in streams.iter_mut().enumerate() {
        if control >> bit & 1 == 1 {
            stream.reset();
        }
    }
    let tp = conv.tpixel_len();
    let (w, h) = (usize::from(rect.width), usize::from(rect.height));

    let (zlib, has_filter) = if control & 0x80 == 0 {
        (true, control & 0x40 != 0)
    } else {
        match control >> 4 {
            0b1000 => {
                let pixel = conv.tpixel(r.take(tp).map_err(short)?);
                end(&r)?;
                fb.fill_rect(rect, pixel);
                return Ok(());
            }
            0b1001 => return Err(DecodeError::Malformed("Tight JPEG was not requested")),
            0b1010 => (false, false),
            0b1110 => (false, true),
            _ => return Err(DecodeError::Malformed("Tight compression control")),
        }
    };
    let filter = if has_filter {
        r.u8().map_err(short)?
    } else {
        0
    };

    let mut palette: Vec<[u8; 4]> = Vec::new();
    let size = match filter {
        0 => w * h * tp,
        2 => {
            if !matches!(conv.bytes_per_pixel(), 2 | 4) {
                return Err(DecodeError::Malformed("Tight gradient needs 16 or 32 bpp"));
            }
            w * h * tp
        }
        1 => {
            let colours = usize::from(r.u8().map_err(short)?) + 1;
            if colours < 2 {
                return Err(DecodeError::Malformed("Tight palette of one colour"));
            }
            for entry in r.take(colours * tp).map_err(short)?.chunks_exact(tp) {
                palette.push(conv.tpixel(entry));
            }
            if colours == 2 {
                w.div_ceil(8) * h
            } else {
                w * h
            }
        }
        _ => return Err(DecodeError::Malformed("Tight filter")),
    };

    let inflated;
    let data: &[u8] = if zlib && size >= MIN_COMPRESSED {
        let len = compact_length(&mut r).map_err(short)?;
        let compressed = r.take(len).map_err(short)?;
        let stream = &mut streams[usize::from(control >> 4 & 3)];
        inflated = stream.inflate(compressed, size)?;
        if inflated.len() != size {
            return Err(DecodeError::Malformed("Tight data has the wrong size"));
        }
        &inflated
    } else {
        r.take(size).map_err(short)?
    };
    end(&r)?;

    paint(fb, conv, rect, filter, &palette, data)
}

/// Write the filtered `data` into the rectangle.
fn paint(
    fb: &mut Framebuffer,
    conv: &PixelConverter,
    rect: Rect,
    filter: u8,
    palette: &[[u8; 4]],
    data: &[u8],
) -> Result<(), DecodeError> {
    let (w, tp) = (usize::from(rect.width), conv.tpixel_len());
    match filter {
        0 => {
            for row in 0..rect.height {
                let src = &data[usize::from(row) * w * tp..][..w * tp];
                for (dst, px) in fb
                    .row_mut(rect, row)
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.chunks_exact(tp))
                {
                    dst.copy_from_slice(&conv.tpixel(px));
                }
            }
        }
        1 => {
            let packed = palette.len() == 2;
            let stride = if packed { w.div_ceil(8) } else { w };
            for row in 0..rect.height {
                let src = &data[usize::from(row) * stride..][..stride];
                for (x, dst) in fb
                    .row_mut(rect, row)
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .enumerate()
                {
                    let index = if packed {
                        usize::from(src[x / 8] >> (7 - x % 8) & 1)
                    } else {
                        usize::from(src[x])
                    };
                    let colour = palette
                        .get(index)
                        .ok_or(DecodeError::Malformed("Tight palette index"))?;
                    dst.copy_from_slice(colour);
                }
            }
        }
        _ => gradient(fb, conv, rect, data, tp),
    }
    Ok(())
}

fn end(r: &Reader<'_>) -> Result<(), DecodeError> {
    if r.remaining() == 0 {
        Ok(())
    } else {
        Err(DecodeError::Malformed("trailing bytes after Tight data"))
    }
}

/// 1 to 3 bytes, 7 bits each, least significant first.
fn compact_length(r: &mut Reader<'_>) -> Result<usize, rfb_proto::ProtoError> {
    let b0 = usize::from(r.u8()?);
    let mut len = b0 & 0x7F;
    if b0 & 0x80 != 0 {
        let b1 = usize::from(r.u8()?);
        len |= (b1 & 0x7F) << 7;
        if b1 & 0x80 != 0 {
            len |= usize::from(r.u8()?) << 14;
        }
    }
    Ok(len)
}

/// Undo the gradient filter: each component is the sum of the prediction
/// (left + above - above-left, clamped to `0..=max`) and the transmitted
/// difference, modulo `max + 1`. Pixels outside the rectangle count as zero.
fn gradient(fb: &mut Framebuffer, conv: &PixelConverter, rect: Rect, data: &[u8], tp: usize) {
    let (w, h) = (usize::from(rect.width), usize::from(rect.height));
    let max = conv.tpixel_max();
    let mut above = vec![[0u32; 3]; w];
    let mut row_values = vec![[0u32; 3]; w];
    for row in 0..h {
        let src = &data[row * w * tp..][..w * tp];
        let mut left = [0u32; 3];
        let mut above_left = [0u32; 3];
        let dst_row = fb.row_mut(rect, u16::try_from(row).unwrap_or(u16::MAX));
        for (x, (px, dst)) in src
            .chunks_exact(tp)
            .zip(dst_row.as_chunks_mut::<4>().0.iter_mut())
            .enumerate()
        {
            let diff = conv.tpixel_channels(px);
            let mut value = [0u32; 3];
            for k in 0..3 {
                let predicted = (left[k] + above[x][k])
                    .saturating_sub(above_left[k])
                    .min(max[k]);
                value[k] = (diff[k] + predicted) % (max[k] + 1);
            }
            dst.copy_from_slice(&conv.pixel_from_channels(value));
            row_values[x] = value;
            left = value;
            above_left = above[x];
        }
        above.clone_from(&row_values);
    }
}
