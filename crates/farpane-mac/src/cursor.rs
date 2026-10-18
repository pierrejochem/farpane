//! Cursor bitmaps from macOS to the shape the server sends.
//!
//! `NSCursor` images are premultiplied (colour already multiplied by alpha);
//! the Cursor pseudo-encoding wants straight RGBA. The hotspot is in points,
//! the image in pixels.

use rfb_codec::Cursor;

/// Largest cursor accepted (pixels per side); bigger ones are not sent.
const MAX_SIDE: usize = 512;

/// Channel order of the source bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Rgba,
    Bgra,
}

/// A cursor with no pixels: the server hides the pointer.
#[must_use]
pub fn hidden() -> Cursor {
    Cursor {
        hotspot: (0, 0),
        width: 0,
        height: 0,
        rgba: Vec::new(),
    }
}

/// Convert a premultiplied `width` x `height` bitmap (rows `stride` bytes
/// apart) with its hotspot in points and `scale` pixels per point. `None` if
/// the data is too short, the stride too small, or the image over 512 pixels
/// a side.
#[must_use]
pub fn from_premultiplied(
    src: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    order: Order,
    hotspot_points: (f64, f64),
    scale: f64,
) -> Option<Cursor> {
    let row = width.checked_mul(4)?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE || stride < row {
        return None;
    }
    if src.len() < (height - 1).checked_mul(stride)?.checked_add(row)? {
        return None;
    }
    let mut rgba = Vec::with_capacity(row * height);
    for y in 0..height {
        for px in src[y * stride..y * stride + row].as_chunks::<4>().0 {
            let (r, g, b) = match order {
                Order::Rgba => (px[0], px[1], px[2]),
                Order::Bgra => (px[2], px[1], px[0]),
            };
            let a = px[3];
            let straight = |c: u8| {
                if a == 0 {
                    0
                } else {
                    // Rounded division, capped: a damaged premultiplied value
                    // above alpha must not wrap.
                    u8::try_from((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a))
                        .unwrap_or(255)
                }
            };
            rgba.extend_from_slice(&[straight(r), straight(g), straight(b), a]);
        }
    }
    let point = |v: f64, max: usize| {
        let v = (v * scale)
            .round()
            .clamp(0.0, f64::from(u16::try_from(max - 1).unwrap_or(u16::MAX)));
        // Clamped into 0..max, and max is at most 512.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = v as u16;
        v
    };
    // The size checks above bound both to 512.
    #[allow(clippy::cast_possible_truncation)]
    Some(Cursor {
        hotspot: (
            point(hotspot_points.0, width),
            point(hotspot_points.1, height),
        ),
        width: width as u16,
        height: height as u16,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_colour_is_divided_by_alpha() {
        // One pixel: 50% alpha, premultiplied red 64 (straight 128), plus a transparent one.
        let src = [64, 0, 0, 128, 9, 9, 9, 0];
        let c = from_premultiplied(&src, 2, 1, 8, Order::Rgba, (0.0, 0.0), 1.0).unwrap();
        assert_eq!(c.rgba, [128, 0, 0, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn bgra_is_reordered_and_opaque_pixels_are_unchanged() {
        let src = [10, 20, 30, 255];
        let c = from_premultiplied(&src, 1, 1, 4, Order::Bgra, (0.0, 0.0), 1.0).unwrap();
        assert_eq!(c.rgba, [30, 20, 10, 255]);
    }

    #[test]
    fn a_value_above_alpha_is_capped_not_wrapped() {
        let src = [200, 0, 0, 100];
        let c = from_premultiplied(&src, 1, 1, 4, Order::Rgba, (0.0, 0.0), 1.0).unwrap();
        assert_eq!(c.rgba[0], 255);
    }

    #[test]
    fn stride_padding_is_skipped_and_the_hotspot_is_scaled_and_clamped() {
        let mut src = vec![0xAA; 2 * 12];
        for px in src.as_chunks_mut::<4>().0.iter_mut().take(2) {
            px.copy_from_slice(&[1, 2, 3, 255]);
        }
        src[12..20].copy_from_slice(&[4, 5, 6, 255, 7, 8, 9, 255]);
        let c = from_premultiplied(&src, 2, 2, 12, Order::Rgba, (0.5, 5.0), 2.0).unwrap();
        assert_eq!(&c.rgba[8..12], [4, 5, 6, 255]);
        assert_eq!(
            c.hotspot,
            (1, 1),
            "0.5 pt = 1 px; 5 pt = 10 px clamps to the last row"
        );
        assert_eq!((c.width, c.height), (2, 2));
    }

    #[test]
    fn bad_sizes_are_refused() {
        assert!(from_premultiplied(&[0; 4], 0, 1, 4, Order::Rgba, (0.0, 0.0), 1.0).is_none());
        assert!(from_premultiplied(&[0; 4], 2, 1, 8, Order::Rgba, (0.0, 0.0), 1.0).is_none());
        assert!(from_premultiplied(&[0; 8], 2, 1, 4, Order::Rgba, (0.0, 0.0), 1.0).is_none());
        assert!(from_premultiplied(&[], 513, 1, 2052, Order::Rgba, (0.0, 0.0), 1.0).is_none());
        assert_eq!(hidden().rgba.len(), 0);
    }
}
