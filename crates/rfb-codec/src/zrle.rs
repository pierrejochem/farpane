//! ZRLE: zlib-compressed 64x64 tiles (see [`crate::tiles`]).

use rfb_proto::{types::Rect, wire::Reader};

use crate::{DecodeError, Framebuffer, PixelConverter, inflate::Inflater, tiles};

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
    let tile_count = w.div_ceil(usize::from(TILE)) * h.div_ceil(usize::from(TILE));
    // Worst case: every pixel as its own run plus a full palette per tile.
    let limit = w * h * (cpix + 1) + tile_count * (2 + 128 * cpix);
    let data = inflater.inflate(&payload[4..], limit)?;
    tiles::decode_tiles(fb, conv, rect, &data, TILE, false)
}
