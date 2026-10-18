//! Frames from `ScreenCaptureKit` to what the server wants.
//!
//! A captured frame is `BGRA` (the byte order the framebuffer uses already)
//! but its rows are padded to a stride, and the system names the changed
//! areas as floating-point rectangles.

use rfb_proto::types::Rect;

/// More rectangles than this are merged into their bounding box: a long list
/// costs the clients more than the pixels it saves.
const MAX_RECTS: usize = 64;

/// Copy `height` rows of `width` pixels out of `src`, whose rows are `stride`
/// bytes apart, into one buffer with no padding. `None` if `src` is too
/// short or the stride is smaller than a row.
#[must_use]
pub fn pack_rows(src: &[u8], stride: usize, width: usize, height: usize) -> Option<Vec<u8>> {
    let row = width.checked_mul(4)?;
    if stride < row
        || (height > 0 && src.len() < (height - 1).checked_mul(stride)?.checked_add(row)?)
    {
        return None;
    }
    let mut out = Vec::with_capacity(row * height);
    for y in 0..height {
        out.extend_from_slice(&src[y * stride..y * stride + row]);
    }
    Some(out)
}

/// RFB rectangles for the system's dirty rectangles (`x`, `y`, width, height
/// in pixels, fractional): rounded outwards, clipped to the `width` x
/// `height` picture, empty ones dropped.
#[must_use]
pub fn dirty_rects(dirty: &[(f64, f64, f64, f64)], width: u16, height: u16) -> Vec<Rect> {
    let clip = |v: f64, max: u16| {
        let v = v.clamp(0.0, f64::from(max));
        // Clamped into u16 range.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = v as u16;
        v
    };
    let mut out: Vec<Rect> = dirty
        .iter()
        .filter(|(x, y, w, h)| [x, y, w, h].iter().all(|v| v.is_finite()))
        .filter_map(|&(x, y, w, h)| {
            let (x0, y0) = (clip(x.floor(), width), clip(y.floor(), height));
            let (x1, y1) = (clip((x + w).ceil(), width), clip((y + h).ceil(), height));
            (x1 > x0 && y1 > y0).then_some(Rect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            })
        })
        .collect();
    if out.len() > MAX_RECTS {
        let x0 = out.iter().map(|r| r.x).min().unwrap_or(0);
        let y0 = out.iter().map(|r| r.y).min().unwrap_or(0);
        let x1 = out.iter().map(|r| r.x + r.width).max().unwrap_or(0);
        let y1 = out.iter().map(|r| r.y + r.height).max().unwrap_or(0);
        out = vec![Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_rows_are_packed() {
        // 3 x 2 pixels, stride 16 (4 bytes of padding per row).
        let mut src = vec![0xEE; 16 * 2];
        for (i, b) in src[..12].iter_mut().enumerate() {
            *b = u8::try_from(i).unwrap();
        }
        for (i, b) in src[16..28].iter_mut().enumerate() {
            *b = 100 + u8::try_from(i).unwrap();
        }
        let out = pack_rows(&src, 16, 3, 2).unwrap();
        assert_eq!(out.len(), 24);
        assert_eq!(&out[..12], &(0..12).collect::<Vec<u8>>()[..]);
        assert_eq!(out[12], 100);
        assert!(!out.contains(&0xEE));
    }

    #[test]
    fn short_buffers_and_small_strides_are_refused() {
        assert!(
            pack_rows(&[0; 20], 16, 3, 2).is_none(),
            "second row incomplete"
        );
        assert!(pack_rows(&[0; 64], 8, 3, 2).is_none(), "stride under a row");
        // The last row needs no padding after it.
        assert!(pack_rows(&[0; 28], 16, 3, 2).is_some());
        assert_eq!(pack_rows(&[], 16, 3, 0).unwrap(), Vec::<u8>::new());
        assert!(
            pack_rows(&[0; 4], usize::MAX, usize::MAX, 2).is_none(),
            "no overflow"
        );
    }

    #[test]
    fn dirty_rects_round_outwards_and_clip() {
        let rects = dirty_rects(
            &[
                (10.4, 20.6, 5.0, 5.0),
                (-5.0, -5.0, 10.0, 10.0),
                (95.0, 95.0, 50.0, 50.0),
            ],
            100,
            100,
        );
        assert_eq!(
            rects,
            [
                Rect {
                    x: 10,
                    y: 20,
                    width: 6,
                    height: 6
                },
                Rect {
                    x: 0,
                    y: 0,
                    width: 5,
                    height: 5
                },
                Rect {
                    x: 95,
                    y: 95,
                    width: 5,
                    height: 5
                },
            ]
        );
    }

    #[test]
    fn empty_and_bad_rects_are_dropped() {
        assert_eq!(
            dirty_rects(
                &[
                    (10.0, 10.0, 0.0, 5.0),
                    (f64::NAN, 0.0, 5.0, 5.0),
                    (200.0, 0.0, 5.0, 5.0)
                ],
                100,
                100
            ),
            []
        );
    }

    #[test]
    fn many_rectangles_become_their_bounding_box() {
        let many: Vec<_> = (0..65)
            .map(|i| (f64::from(i), f64::from(i), 1.0, 1.0))
            .collect();
        assert_eq!(
            dirty_rects(&many, 200, 200),
            [Rect {
                x: 0,
                y: 0,
                width: 65,
                height: 65
            }]
        );
    }
}
