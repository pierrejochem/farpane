//! TRLE: 16x16 tiles without zlib (RFC 6143 section 7.7.5; see
//! [`crate::tiles`]). Palettes may be reused from the previous tile.

use rfb_proto::types::Rect;

use crate::{DecodeError, Framebuffer, PixelConverter, tiles};

const TILE: u16 = 16;

pub(crate) fn decode(
    fb: &mut Framebuffer,
    conv: &PixelConverter,
    rect: Rect,
    payload: &[u8],
) -> Result<(), DecodeError> {
    fb.check(rect)?;
    tiles::decode_tiles(fb, conv, rect, payload, TILE, true)
}
