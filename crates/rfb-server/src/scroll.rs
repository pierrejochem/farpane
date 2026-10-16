//! Finding scrolled content: parts of the new picture that equal the old
//! picture moved a few rows up or down, so clients can be told to copy them
//! (`CopyRect`) instead of receiving the pixels again.
#![allow(
    clippy::many_single_char_names, // rectangle and row arithmetic
    clippy::cast_possible_wrap,     // rows and shifts are far below isize::MAX
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::needless_range_loop // `r` is also the row number
)]

use rfb_proto::types::Rect;

/// Smallest block of rows worth a copy.
const MIN_ROWS: usize = 24;
/// Narrowest rectangle worth looking at.
const MIN_WIDTH: usize = 48;
/// Most rows content is looked for to have moved.
const MAX_SHIFT: usize = 600;
/// Rows that all look alike (blank paper) match any shift; a copy needs at
/// least this many different rows.
const MIN_DISTINCT_ROWS: usize = 6;

/// "Copy the pixels at (`src_x`, `src_y`) to `dst`" (same size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copy {
    pub src_x: u16,
    pub src_y: u16,
    pub dst: Rect,
}

fn row_hash(pixels: &[u8], stride: usize, x: usize, y: usize, w: usize) -> u64 {
    let at = y * stride + x * 4;
    // FNV-1a over the colour bytes (the alpha byte is always opaque).
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for px in pixels[at..at + w * 4].chunks_exact(4) {
        for &b in &px[..3] {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn rows_equal(
    old: &[u8],
    new: &[u8],
    stride: usize,
    x: usize,
    old_y: usize,
    new_y: usize,
    w: usize,
) -> bool {
    let (a, b) = (old_y * stride + x * 4, new_y * stride + x * 4);
    old[a..a + w * 4]
        .chunks_exact(4)
        .zip(new[b..b + w * 4].chunks_exact(4))
        .all(|(o, n)| o[..3] == n[..3])
}

/// Find the vertical scroll in `rect`, if any: `old` and `new` are the whole
/// pictures (`B, G, R, X` rows of `stride` bytes). Returns at most one copy
/// (the longest run of rows that moved by one common amount).
#[must_use]
pub fn find_copy(old: &[u8], new: &[u8], stride: usize, rect: Rect) -> Option<Copy> {
    let (x, y0) = (usize::from(rect.x), usize::from(rect.y));
    let (w, h) = (usize::from(rect.width), usize::from(rect.height));
    if w < MIN_WIDTH || h < MIN_ROWS * 2 {
        return None;
    }
    let old_hash: Vec<u64> = (0..h)
        .map(|r| row_hash(old, stride, x, y0 + r, w))
        .collect();
    let new_hash: Vec<u64> = (0..h)
        .map(|r| row_hash(new, stride, x, y0 + r, w))
        .collect();
    // The shift with the most matching rows. `shift > 0`: content moved down,
    // so new row r equals old row r - shift.
    let mut best: Option<(usize, isize)> = None;
    for shift in 1..=MAX_SHIFT.min(h - MIN_ROWS) {
        for sign in [1isize, -1] {
            let d = shift as isize * sign;
            let mut matches = 0usize;
            for r in 0..h {
                let Ok(src) = usize::try_from(r as isize - d) else {
                    continue;
                };
                if src < h && new_hash[r] == old_hash[src] {
                    matches += 1;
                }
            }
            if best.is_none_or(|(m, _)| matches > m) {
                best = Some((matches, d));
            }
        }
    }
    let (matches, d) = best?;
    if matches < MIN_ROWS {
        return None;
    }
    // The longest run of consecutive rows that match at that shift, checked
    // against the real pixels (hashes can collide).
    let mut run = (0usize, 0usize); // (start, length)
    let mut start = None;
    for r in 0..=h {
        let ok = r < h
            && usize::try_from(r as isize - d).is_ok_and(|src| {
                src < h
                    && new_hash[r] == old_hash[src]
                    && rows_equal(old, new, stride, x, y0 + src, y0 + r, w)
            });
        match (ok, start) {
            (true, None) => start = Some(r),
            (false, Some(s)) => {
                if r - s > run.1 {
                    run = (s, r - s);
                }
                start = None;
            }
            _ => {}
        }
    }
    if run.1 < MIN_ROWS {
        return None;
    }
    // Blank rows match everything: ask for some variety.
    let distinct = {
        let mut seen: Vec<u64> = new_hash[run.0..run.0 + run.1].to_vec();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    };
    if distinct < MIN_DISTINCT_ROWS {
        return None;
    }
    let src_row = usize::try_from((run.0 + y0) as isize - d).ok()?;
    Some(Copy {
        src_x: rect.x,
        src_y: u16::try_from(src_row).ok()?,
        dst: Rect {
            x: rect.x,
            y: u16::try_from(y0 + run.0).ok()?,
            width: rect.width,
            height: u16::try_from(run.1).ok()?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 120;
    const H: usize = 200;

    /// A picture whose every row is different.
    fn page(first_line: usize, rows: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for r in 0..rows {
            for x in 0..W {
                let v = (first_line + r) * 31 + x * 7;
                out.extend_from_slice(&[v as u8, (v >> 8) as u8 ^ 0x5A, (v / 3) as u8, 255]);
            }
        }
        out
    }

    fn whole() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: W as u16,
            height: H as u16,
        }
    }

    #[test]
    fn scrolled_text_is_found_with_its_direction_and_distance() {
        // The page scrolled up by 30 lines: new row r shows old row r + 30.
        let old = page(0, H);
        let new = page(30, H);
        let c = find_copy(&old, &new, W * 4, whole()).expect("a scroll");
        // The first H - 30 rows of the new picture come from 30 rows lower.
        assert_eq!((c.src_x, c.src_y), (0, 30));
        assert_eq!(
            c.dst,
            Rect {
                x: 0,
                y: 0,
                width: W as u16,
                height: (H - 30) as u16
            }
        );
        // The other way: scrolled down by 45 lines.
        let new = page(0, H);
        let old = page(45, H);
        let c = find_copy(&old, &new, W * 4, whole()).expect("a scroll");
        assert_eq!((c.src_y, c.dst.y), (0, 45));
        assert_eq!(c.dst.height, (H - 45) as u16);
    }

    #[test]
    fn what_did_not_move_or_is_blank_is_not_a_scroll() {
        let a = page(0, H);
        assert_eq!(find_copy(&a, &a, W * 4, whole()), None, "nothing changed");
        let unrelated = page(1000, H);
        assert_eq!(find_copy(&a, &unrelated, W * 4, whole()), None);
        // Blank pages match at every shift: no copy.
        let blank = vec![255u8; W * H * 4];
        assert_eq!(find_copy(&blank, &blank, W * 4, whole()), None);
        // Too small to bother.
        let small = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        };
        assert_eq!(find_copy(&a, &page(3, H), W * 4, small), None);
    }

    #[test]
    fn a_scroll_inside_a_region_with_a_changed_strip_still_finds_the_moved_part() {
        let old = page(0, H);
        let mut new = page(30, H);
        // A status bar at the bottom changed on its own.
        for r in (H - 20)..H {
            for x in 0..W {
                new[(r * W + x) * 4] ^= 0xFF;
            }
        }
        let c = find_copy(&old, &new, W * 4, whole()).expect("a scroll");
        // New rows 0..170 come from old rows 30..200; the strip at the bottom
        // (rows 180..200) is outside that and left to normal encoding.
        assert_eq!(c.src_y, 30);
        assert_eq!((c.dst.y, c.dst.height), (0, 170), "{c:?}");
    }
}
