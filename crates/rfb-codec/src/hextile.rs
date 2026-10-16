//! Hextile: 16x16 tiles, each raw or a background with subrectangles.
//!
//! Background and foreground colours carry over between tiles. The spec says
//! some carry-overs are invalid (after a raw tile, after coloured
//! subrectangles); like `TigerVNC` we accept them and reuse the last colour that
//! was specified. Using a colour that was never specified is an error.

use rfb_proto::{types::Rect, wire::Reader};

use crate::{DecodeError, Framebuffer, PixelConverter};

const RAW: u8 = 1;
const BACKGROUND_SPECIFIED: u8 = 2;
const FOREGROUND_SPECIFIED: u8 = 4;
const ANY_SUBRECTS: u8 = 8;
const SUBRECTS_COLOURED: u8 = 16;

const TILE: u16 = 16;

pub(crate) fn decode(
    fb: &mut Framebuffer,
    conv: &PixelConverter,
    rect: Rect,
    payload: &[u8],
) -> Result<(), DecodeError> {
    fb.check(rect)?;
    let bpp = conv.bytes_per_pixel();
    let short = |_| DecodeError::Malformed("truncated Hextile payload");
    let mut reader = Reader::new(payload);
    let (mut background, mut foreground): (Option<[u8; 4]>, Option<[u8; 4]>) = (None, None);

    for ty in (0..rect.height).step_by(usize::from(TILE)) {
        let th = (rect.height - ty).min(TILE);
        for tx in (0..rect.width).step_by(usize::from(TILE)) {
            let tw = (rect.width - tx).min(TILE);
            let tile = Rect {
                x: rect.x + tx,
                y: rect.y + ty,
                width: tw,
                height: th,
            };
            let mask = reader.u8().map_err(short)?;
            if mask & RAW != 0 {
                let row_bytes = usize::from(tw) * bpp;
                for row in 0..th {
                    let src = reader.take(row_bytes).map_err(short)?;
                    conv.convert_row(src, fb.row_mut(tile, row));
                }
                continue;
            }
            if mask & BACKGROUND_SPECIFIED != 0 {
                background = Some(conv.pixel(reader.take(bpp).map_err(short)?));
            }
            if mask & FOREGROUND_SPECIFIED != 0 {
                if mask & SUBRECTS_COLOURED != 0 {
                    return Err(DecodeError::Malformed(
                        "Hextile foreground with coloured subrectangles",
                    ));
                }
                foreground = Some(conv.pixel(reader.take(bpp).map_err(short)?));
            }
            let bg =
                background.ok_or(DecodeError::Malformed("Hextile background not specified"))?;
            fb.fill_rect(tile, bg);
            if mask & ANY_SUBRECTS == 0 {
                continue;
            }
            let count = reader.u8().map_err(short)?;
            for _ in 0..count {
                let pixel = if mask & SUBRECTS_COLOURED != 0 {
                    conv.pixel(reader.take(bpp).map_err(short)?)
                } else {
                    foreground.ok_or(DecodeError::Malformed("Hextile foreground not specified"))?
                };
                let xy = reader.u8().map_err(short)?;
                let wh = reader.u8().map_err(short)?;
                let (sx, sy) = (u16::from(xy >> 4), u16::from(xy & 0xF));
                let (sw, sh) = (u16::from(wh >> 4) + 1, u16::from(wh & 0xF) + 1);
                if sx + sw > tw || sy + sh > th {
                    return Err(DecodeError::Malformed("Hextile subrectangle outside tile"));
                }
                fb.fill_rect(
                    Rect {
                        x: tile.x + sx,
                        y: tile.y + sy,
                        width: sw,
                        height: sh,
                    },
                    pixel,
                );
            }
        }
    }
    if reader.remaining() != 0 {
        return Err(DecodeError::Malformed(
            "trailing bytes after Hextile payload",
        ));
    }
    Ok(())
}
