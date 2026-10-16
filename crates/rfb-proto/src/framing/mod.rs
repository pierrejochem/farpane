//! Rectangle framing: find where a rectangle payload ends without decoding
//! pixels.
//!
//! Rectangle payloads are not self-delimiting (RFB spec, "Encodings"). `frame`
//! scans just enough of the bytes after a [`RectHeader`] to learn the payload
//! length. Decoding is `rfb-codec`'s job and sees only complete payloads.
//!
//! Handled: Raw, `CopyRect`, RRE, `CoRRE`, Hextile, zlib, ZRLE and the
//! `Cursor`, `DesktopSize` and `LastRect` pseudo-encodings. `ZRLE` and zlib are
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
        enc::ZLIB | enc::ZRLE => length_prefixed(input),
        pseudo::CURSOR => fixed(
            width * height * bpp as u64 + width.div_ceil(8) * height,
            input,
        ),
        pseudo::DESKTOP_SIZE | pseudo::LAST_RECT => Ok(0),
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
fn skip_bounded(r: &mut Reader<'_>, total: &mut u64, n: usize) -> Result<(), ProtoError> {
    *total += n as u64;
    check_limit(*total, "Hextile payload")?;
    r.skip(n)
}

#[cfg(test)]
mod tests;
