//! RRE and `CoRRE`: a background colour plus a list of solid subrectangles.

use rfb_proto::{types::Rect, wire::Reader};

use crate::{DecodeError, Framebuffer, PixelConverter};

/// Decode an RRE (`narrow == false`, `U16` coordinates) or `CoRRE`
/// (`narrow == true`, `U8` coordinates) rectangle.
pub(crate) fn decode(
    fb: &mut Framebuffer,
    conv: &PixelConverter,
    rect: Rect,
    payload: &[u8],
    narrow: bool,
) -> Result<(), DecodeError> {
    fb.check(rect)?;
    let bpp = conv.bytes_per_pixel();
    let mut reader = Reader::new(payload);
    let short = |_| DecodeError::Malformed("truncated RRE payload");
    let count = reader.u32().map_err(short)?;
    let background = conv.pixel(reader.take(bpp).map_err(short)?);
    fb.fill_rect(rect, background);
    for _ in 0..count {
        let pixel = conv.pixel(reader.take(bpp).map_err(short)?);
        let sub = if narrow {
            let b = reader.array::<4>().map_err(short)?;
            [b[0], b[1], b[2], b[3]].map(u16::from)
        } else {
            [
                reader.u16().map_err(short)?,
                reader.u16().map_err(short)?,
                reader.u16().map_err(short)?,
                reader.u16().map_err(short)?,
            ]
        };
        let [sx, sy, sw, sh] = sub;
        if u32::from(sx) + u32::from(sw) > u32::from(rect.width)
            || u32::from(sy) + u32::from(sh) > u32::from(rect.height)
        {
            return Err(DecodeError::Malformed("RRE subrectangle outside rectangle"));
        }
        fb.fill_rect(
            Rect {
                x: rect.x + sx,
                y: rect.y + sy,
                width: sw,
                height: sh,
            },
            pixel,
        );
    }
    if reader.remaining() != 0 {
        return Err(DecodeError::Malformed("trailing bytes after RRE payload"));
    }
    Ok(())
}
