//! Rectangle framing: find where a rectangle payload ends without decoding
//! pixels.
//!
//! Rectangle payloads are not self-delimiting (RFB spec, "Encodings"). `frame`
//! scans just enough of the bytes after a [`RectHeader`] to learn the payload
//! length. Decoding is `rfb-codec`'s job and sees only complete payloads.
//!
//! Handled: Raw, `CopyRect`, RRE, `CoRRE`, Hextile, zlib, ZRLE, TRLE, Tight and the
//! `Cursor`, `DesktopSize`, `ExtendedDesktopSize` and `LastRect`
//! pseudo-encodings. `ZRLE` and zlib are
//! length-prefixed, so their tiles are walked only after inflating, in the
//! codec. Anything else is fatal: the stream cannot be resynchronised.

use crate::{
    ProtoError,
    consts::{enc, limits, pseudo},
    msg::server::RectHeader,
    types::PixelFormat,
    wire::Reader,
};

/// Hextile sub-encoding mask bits.
mod hextile {
    pub const RAW: u8 = 1;
    pub const BACKGROUND_SPECIFIED: u8 = 2;
    pub const FOREGROUND_SPECIFIED: u8 = 4;
    pub const ANY_SUBRECTS: u8 = 8;
    pub const SUBRECTS_COLOURED: u8 = 16;
}

/// Length in bytes of the payload that follows `header`, given the pixel
/// format in force.
///
/// `input` starts right after the rectangle header. On success the payload is
/// `input[..n]` for the returned `n`.
///
/// # Errors
/// [`ProtoError::NeedMore`] with the minimum number of extra bytes needed
/// (more may be needed after those arrive), [`ProtoError::LimitExceeded`] if
/// the payload would exceed [`limits::MAX_RECT_PAYLOAD`],
/// [`ProtoError::Malformed`] on an invalid Hextile tile, and
/// [`ProtoError::UnsupportedEncoding`] for any other encoding.
pub fn frame(header: &RectHeader, pf: &PixelFormat, input: &[u8]) -> Result<usize, ProtoError> {
    let width = u64::from(header.rect.width);
    let height = u64::from(header.rect.height);
    let bpp = pf.bytes_per_pixel();
    match header.encoding {
        enc::RAW => fixed(width * height * bpp as u64, input),
        enc::COPY_RECT => fixed(4, input),
        enc::RRE => rre(input, bpp as u64, 8),
        enc::CO_RRE => rre(input, bpp as u64, 4),
        enc::HEXTILE => hextile(input, header.rect.width, header.rect.height, bpp),
        enc::TRLE => trle(input, header.rect.width, header.rect.height, pf),
        enc::TIGHT => tight(
            input,
            header.rect.width,
            header.rect.height,
            pf.tight_pixel_len(),
        ),
        enc::ZLIB | enc::ZRLE => length_prefixed(input),
        pseudo::CURSOR => fixed(
            width * height * bpp as u64 + width.div_ceil(8) * height,
            input,
        ),
        pseudo::DESKTOP_SIZE | pseudo::LAST_RECT => Ok(0),
        // U8 screen count, 3 padding bytes, then 16 bytes per screen.
        pseudo::EXT_DESKTOP_SIZE => {
            let count = u64::from(Reader::new(input).u8()?);
            fixed(4 + 16 * count, input)
        }
        other => Err(ProtoError::UnsupportedEncoding(other)),
    }
}

fn check_limit(len: u64, field: &'static str) -> Result<usize, ProtoError> {
    match u32::try_from(len) {
        Ok(n) if n <= limits::MAX_RECT_PAYLOAD => Ok(n as usize),
        _ => Err(ProtoError::LimitExceeded {
            field,
            value: len,
            limit: u64::from(limits::MAX_RECT_PAYLOAD),
        }),
    }
}

/// A payload whose total length is known up front.
fn fixed(len: u64, input: &[u8]) -> Result<usize, ProtoError> {
    let len = check_limit(len, "rectangle payload")?;
    match len.checked_sub(input.len()) {
        Some(missing) if missing > 0 => Err(ProtoError::NeedMore(missing)),
        _ => Ok(len),
    }
}

/// `U32 length` followed by that many bytes (zlib, ZRLE).
fn length_prefixed(input: &[u8]) -> Result<usize, ProtoError> {
    let len = u64::from(Reader::new(input).u32()?);
    fixed(4 + len, input)
}

/// RRE and `CoRRE`: `U32 count`, background pixel, then `count` subrectangles of
/// `bpp + coords` bytes each.
fn rre(input: &[u8], bpp: u64, coords: u64) -> Result<usize, ProtoError> {
    let count = u64::from(Reader::new(input).u32()?);
    fixed(4 + bpp + count * (bpp + coords), input)
}

/// Walk 16x16 tiles. Only the byte counts matter, not the colour carry-over
/// rules, so no tile state is kept.
fn hextile(input: &[u8], width: u16, height: u16, bpp: usize) -> Result<usize, ProtoError> {
    let mut r = Reader::new(input);
    let mut total = 0u64;
    for ty in (0..height).step_by(16) {
        let th = usize::from((height - ty).min(16));
        for tx in (0..width).step_by(16) {
            let tw = usize::from((width - tx).min(16));
            let mask = r.u8()?;
            if mask & hextile::RAW != 0 {
                skip_bounded(&mut r, &mut total, tw * th * bpp)?;
                continue;
            }
            let mut n = 0;
            if mask & hextile::BACKGROUND_SPECIFIED != 0 {
                n += bpp;
            }
            if mask & hextile::FOREGROUND_SPECIFIED != 0 {
                if mask & hextile::SUBRECTS_COLOURED != 0 {
                    return Err(ProtoError::Malformed {
                        ctx: "Hextile foreground with coloured subrects",
                    });
                }
                n += bpp;
            }
            skip_bounded(&mut r, &mut total, n)?;
            if mask & hextile::ANY_SUBRECTS != 0 {
                let subrects = usize::from(r.u8()?);
                let each = if mask & hextile::SUBRECTS_COLOURED != 0 {
                    bpp + 2
                } else {
                    2
                };
                skip_bounded(&mut r, &mut total, subrects * each)?;
            }
        }
    }
    Ok(r.position())
}

/// Skip `n` payload bytes, keeping a running total under the payload limit.
/// TRLE (RFC 6143 section 7.7.5): 16 x 16 tiles, each with a subencoding
/// byte. Only the sizes matter here, but the palette size of the last tile
/// that had one must be remembered, because subencodings 127 and 129 reuse it.
fn trle(input: &[u8], width: u16, height: u16, pf: &PixelFormat) -> Result<usize, ProtoError> {
    let bad = |ctx| ProtoError::Malformed { ctx };
    let cp = pf.cpixel_len();
    let mut r = Reader::new(input);
    let mut total = 0u64;
    // Palette of the last tile that had one (2 to 127 colours); none at the
    // start of a rectangle.
    let mut last_palette: Option<usize> = None;
    for ty in (0..height).step_by(16) {
        let th = usize::from((height - ty).min(16));
        for tx in (0..width).step_by(16) {
            let tw = usize::from((width - tx).min(16));
            let sub = r.u8()?;
            let palette_rle = |r: &mut Reader<'_>, total: &mut u64, n: usize| {
                // Runs until the tile's pixels are used up.
                let mut pixels = 0;
                while pixels < tw * th {
                    let index = r.u8()?;
                    if usize::from(index & 0x7F) >= n {
                        return Err(bad("TRLE palette index"));
                    }
                    let run = if index & 0x80 != 0 {
                        run_length(r, total)?
                    } else {
                        1
                    };
                    pixels += run;
                }
                if pixels == tw * th {
                    Ok(())
                } else {
                    Err(bad("TRLE run past the end of the tile"))
                }
            };
            match sub {
                0 => skip_bounded(&mut r, &mut total, tw * th * cp)?,
                1 => skip_bounded(&mut r, &mut total, cp)?,
                2..=16 | 127 => {
                    let n = if sub == 127 {
                        last_palette.ok_or_else(|| bad("TRLE palette reused before any"))?
                    } else {
                        usize::from(sub)
                    };
                    if !(2..=16).contains(&n) {
                        return Err(bad("TRLE packed palette reuse of a larger palette"));
                    }
                    if sub != 127 {
                        skip_bounded(&mut r, &mut total, n * cp)?;
                        last_palette = Some(n);
                    }
                    let bits = match n {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    skip_bounded(&mut r, &mut total, (tw * bits).div_ceil(8) * th)?;
                }
                17..=126 => return Err(bad("TRLE subencoding")),
                128 => {
                    let mut pixels = 0;
                    while pixels < tw * th {
                        skip_bounded(&mut r, &mut total, cp)?;
                        pixels += run_length(&mut r, &mut total)?;
                    }
                    if pixels != tw * th {
                        return Err(bad("TRLE run past the end of the tile"));
                    }
                }
                129 => {
                    let n = last_palette.ok_or_else(|| bad("TRLE palette reused before any"))?;
                    palette_rle(&mut r, &mut total, n)?;
                }
                130..=255 => {
                    let n = usize::from(sub - 128);
                    skip_bounded(&mut r, &mut total, n * cp)?;
                    last_palette = Some(n);
                    palette_rle(&mut r, &mut total, n)?;
                }
            }
        }
    }
    Ok(r.position())
}

/// A run length: one plus the sum of bytes up to and including the first
/// byte below 255.
fn run_length(r: &mut Reader<'_>, total: &mut u64) -> Result<usize, ProtoError> {
    let mut len = 1usize;
    loop {
        let b = r.u8()?;
        *total += 1;
        check_limit(*total, "TRLE payload")?;
        len += usize::from(b);
        if b != 255 {
            return Ok(len);
        }
    }
}

/// Tight (spec, "Tight Encoding"). The control byte says which of fill, JPEG
/// and basic compression follows; for basic compression the size of the
/// filtered data is known from the filter, and data under 12 bytes is sent
/// without zlib.
fn tight(input: &[u8], width: u16, height: u16, tpixel: usize) -> Result<usize, ProtoError> {
    const MIN_COMPRESSED: usize = 12;
    let bad = |ctx| ProtoError::Malformed { ctx };
    let mut r = Reader::new(input);
    let control = r.u8()?;
    let (w, h, tp) = (u64::from(width), u64::from(height), tpixel as u64);
    let (zlib, filter) = if control & 0x80 == 0 {
        (true, control & 0x40 != 0)
    } else {
        match control >> 4 {
            0b1000 => {
                let mut total = 1;
                skip_bounded(&mut r, &mut total, tpixel)?;
                return Ok(r.position());
            }
            0b1001 => {
                let len = compact_length(&mut r)?;
                let mut total = r.position() as u64;
                skip_bounded(&mut r, &mut total, len)?;
                return Ok(r.position());
            }
            0b1010 => (false, false),
            0b1110 => (false, true),
            _ => return Err(bad("Tight compression control")),
        }
    };
    let filter_id = if filter { r.u8()? } else { 0 };
    let mut total = r.position() as u64;
    let data = match filter_id {
        0 | 2 => w * h * tp,
        1 => {
            let colours = u16::from(r.u8()?) + 1;
            if colours < 2 {
                return Err(bad("Tight palette of one colour"));
            }
            total = r.position() as u64;
            skip_bounded(&mut r, &mut total, usize::from(colours) * tpixel)?;
            if colours == 2 {
                w.div_ceil(8) * h
            } else {
                w * h
            }
        }
        _ => return Err(bad("Tight filter")),
    };
    let data = check_limit(data, "Tight payload")?;
    if zlib && data >= MIN_COMPRESSED {
        let len = compact_length(&mut r)?;
        total = r.position() as u64;
        skip_bounded(&mut r, &mut total, len)?;
    } else {
        skip_bounded(&mut r, &mut total, data)?;
    }
    Ok(r.position())
}

/// Tight's 1 to 3 byte length: 7 bits per byte, least significant first, the
/// top bit says another byte follows (the third byte has all 8 bits).
fn compact_length(r: &mut Reader<'_>) -> Result<usize, ProtoError> {
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

fn skip_bounded(r: &mut Reader<'_>, total: &mut u64, n: usize) -> Result<(), ProtoError> {
    *total += n as u64;
    check_limit(*total, "Hextile payload")?;
    r.skip(n)
}

#[cfg(test)]
mod tests;
