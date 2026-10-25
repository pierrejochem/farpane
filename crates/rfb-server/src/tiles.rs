//! Damage for sources that can only hand over whole pictures: compare the
//! new picture with the one in the framebuffer, 64 x 64 pixels at a time, and
//! report the smallest rectangle inside each tile that holds the changes.

use rfb_codec::Framebuffer;
use rfb_proto::types::Rect;

const TILE: usize = 64;

/// For every 64 x 64 tile of `new` (`B, G, R, X`, `w` x `h`, no padding
/// between rows) that differs from the picture in `fb`, the bounding box of
/// the pixels that differ. A blinking caret costs a few pixels, not a tile.
/// Only the colour bytes are compared: the framebuffer's fourth byte is
/// always opaque.
///
/// `fb` must be at least `w` x `h`; `new` must hold `w * h * 4` bytes.
#[must_use]
pub fn changed_tiles(fb: &Framebuffer, new: &[u8], w: usize, h: usize) -> Vec<Rect> {
    let old = fb.pixels();
    let stride = fb.stride();
    let mut out = Vec::new();
    for ty in (0..h).step_by(TILE) {
        let th = (h - ty).min(TILE);
        for tx in (0..w).step_by(TILE) {
            let tw = (w - tx).min(TILE);
            // Bounds of the differing pixels as (left, top, right, bottom),
            // right and bottom exclusive.
            let mut bounds: Option<(usize, usize, usize, usize)> = None;
            for row in 0..th {
                let old_at = (ty + row) * stride + tx * 4;
                let new_at = ((ty + row) * w + tx) * 4;
                let pairs = || {
                    old[old_at..old_at + tw * 4]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(new[new_at..new_at + tw * 4].as_chunks::<4>().0)
                };
                let Some(first) = pairs().position(|(o, n)| o[..3] != n[..3]) else {
                    continue;
                };
                // A first difference exists, so there is a last one.
                let last = pairs()
                    .rposition(|(o, n)| o[..3] != n[..3])
                    .unwrap_or(first);
                bounds = Some(match bounds {
                    None => (first, row, last + 1, row + 1),
                    Some((left, top, right, _)) => {
                        (left.min(first), top, right.max(last + 1), row + 1)
                    }
                });
            }
            if let Some((left, top, right, bottom)) = bounds {
                // Tile sizes are at most 64.
                #[allow(clippy::cast_possible_truncation)]
                out.push(Rect {
                    x: (tx + left) as u16,
                    y: (ty + top) as u16,
                    width: (right - left) as u16,
                    height: (bottom - top) as u16,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: u16, h: u16) -> (Framebuffer, Vec<u8>) {
        let mut wire = Vec::new();
        for y in 0..h {
            for x in 0..w {
                wire.extend_from_slice(&[(x % 251) as u8, (y % 241) as u8, 7, 0]);
            }
        }
        let mut fb = Framebuffer::new(w, h).unwrap();
        let all = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        fb.write_bgrx(all, &wire).unwrap();
        (fb, wire)
    }

    #[test]
    fn only_the_changed_pixels_of_each_tile_are_reported() {
        let (fb, mut new) = picture(150, 70);
        assert_eq!(changed_tiles(&fb, &new, 150, 70), []);
        // One pixel in the third tile of the first row and one in the last tile.
        new[((5 * 150) + 130) * 4] ^= 0x55;
        new[((69 * 150) + 149) * 4 + 2] ^= 0x01;
        assert_eq!(
            changed_tiles(&fb, &new, 150, 70),
            [
                Rect {
                    x: 130,
                    y: 5,
                    width: 1,
                    height: 1
                },
                Rect {
                    x: 149,
                    y: 69,
                    width: 1,
                    height: 1
                },
            ]
        );
    }

    #[test]
    fn the_box_spans_every_changed_pixel_of_the_tile() {
        let (fb, mut new) = picture(64, 64);
        // Two pixels far apart in one tile: the box covers both.
        new[((10 * 64) + 20) * 4] ^= 1;
        new[((40 * 64) + 3) * 4 + 1] ^= 1;
        assert_eq!(
            changed_tiles(&fb, &new, 64, 64),
            [Rect {
                x: 3,
                y: 10,
                width: 18,
                height: 31
            }]
        );
    }

    #[test]
    fn the_alpha_byte_is_ignored() {
        let (fb, mut new) = picture(64, 64);
        for alpha in new.iter_mut().skip(3).step_by(4) {
            *alpha = 0x80;
        }
        assert_eq!(changed_tiles(&fb, &new, 64, 64), []);
    }
}
