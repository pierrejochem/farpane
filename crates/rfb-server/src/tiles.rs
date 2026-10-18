//! Damage for sources that can only hand over whole pictures: compare the
//! new picture with the one in the framebuffer, 64 x 64 pixels at a time.

use rfb_codec::Framebuffer;
use rfb_proto::types::Rect;

const TILE: usize = 64;

/// The 64 x 64 tiles of `new` (`B, G, R, X`, `w` x `h`, no padding between
/// rows) that differ from the picture in `fb`. Only the colour bytes are
/// compared: the framebuffer's fourth byte is always opaque.
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
            let differs = (0..th).any(|row| {
                let a = (ty + row) * stride + tx * 4;
                let b = ((ty + row) * w + tx) * 4;
                old[a..a + tw * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(new[b..b + tw * 4].as_chunks::<4>().0)
                    .any(|(o, n)| o[..3] != n[..3])
            });
            if differs {
                // Tile sizes are at most 64.
                #[allow(clippy::cast_possible_truncation)]
                out.push(Rect {
                    x: tx as u16,
                    y: ty as u16,
                    width: tw as u16,
                    height: th as u16,
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
    fn only_the_tiles_that_changed_are_reported() {
        let (fb, mut new) = picture(150, 70);
        assert_eq!(changed_tiles(&fb, &new, 150, 70), []);
        // One pixel in the third tile of the first row and one in the last tile.
        new[((5 * 150) + 130) * 4] ^= 0x55;
        new[((69 * 150) + 149) * 4 + 2] ^= 0x01;
        assert_eq!(
            changed_tiles(&fb, &new, 150, 70),
            [
                Rect {
                    x: 128,
                    y: 0,
                    width: 22,
                    height: 64
                },
                Rect {
                    x: 128,
                    y: 64,
                    width: 22,
                    height: 6
                },
            ]
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
