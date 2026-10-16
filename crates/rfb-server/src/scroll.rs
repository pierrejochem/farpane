//! Finding scrolled content: parts of the new picture that equal the old
//! picture moved a few rows up or down (or columns left or right), so clients can be told to copy them
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

/// Largest rectangle (in pixels) searched for sideways movement.
const MAX_SIDEWAYS_AREA: usize = 600_000;

/// "Copy the pixels at (`src_x`, `src_y`) to `dst`" (same size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copy {
    pub src_x: u16,
    pub src_y: u16,
    pub dst: Rect,
}

/// Which way content may have moved: along rows (down or up) or along
/// columns (left or right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Vertical,
    Horizontal,
}

/// A hash of line `i` of `rect`: a row for a vertical scroll, a column for a
/// horizontal one.
fn line_hash(pixels: &[u8], stride: usize, rect: Rect, axis: Axis, i: usize) -> u64 {
    let (x, y) = (usize::from(rect.x), usize::from(rect.y));
    let (w, h) = (usize::from(rect.width), usize::from(rect.height));
    // FNV-1a over the colour bytes (the alpha byte is always opaque).
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut take = |at: usize| {
        for &b in &pixels[at..at + 3] {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    match axis {
        Axis::Vertical => {
            for c in 0..w {
                take((y + i) * stride + (x + c) * 4);
            }
        }
        Axis::Horizontal => {
            for r in 0..h {
                take((y + r) * stride + (x + i) * 4);
            }
        }
    }
    hash
}

/// Whether old line `old_i` and new line `new_i` of `rect` are the same pixels.
fn lines_equal(
    old: &[u8],
    new: &[u8],
    stride: usize,
    rect: Rect,
    axis: Axis,
    old_i: usize,
    new_i: usize,
) -> bool {
    let (x, y) = (usize::from(rect.x), usize::from(rect.y));
    let (w, h) = (usize::from(rect.width), usize::from(rect.height));
    let px = |pixels: &[u8], at: usize| [pixels[at], pixels[at + 1], pixels[at + 2]];
    match axis {
        Axis::Vertical => (0..w).all(|c| {
            let (a, b) = (
                (y + old_i) * stride + (x + c) * 4,
                (y + new_i) * stride + (x + c) * 4,
            );
            px(old, a) == px(new, b)
        }),
        Axis::Horizontal => (0..h).all(|r| {
            let (a, b) = (
                (y + r) * stride + (x + old_i) * 4,
                (y + r) * stride + (x + new_i) * 4,
            );
            px(old, a) == px(new, b)
        }),
    }
}

/// Find the vertical scroll in `rect`, if any: `old` and `new` are the whole
/// pictures (`B, G, R, X` rows of `stride` bytes). Returns at most one copy
/// (the longest run of rows that moved by one common amount).
#[must_use]
pub fn find_copy(old: &[u8], new: &[u8], stride: usize, rect: Rect) -> Option<Copy> {
    find_axis(old, new, stride, rect, Axis::Vertical)
}

/// Like [`find_copy`], for content that moved sideways (a horizontally
/// scrolled view, a terminal's columns, a timeline).
#[must_use]
pub fn find_copy_sideways(old: &[u8], new: &[u8], stride: usize, rect: Rect) -> Option<Copy> {
    // Columns are read with a stride, which is slow: a full 1920 x 1080
    // search takes about 36 ms, more than a frame. Big rectangles are
    // usually video or a redraw, not a sideways scroll, so skip them.
    if usize::from(rect.width) * usize::from(rect.height) > MAX_SIDEWAYS_AREA {
        return None;
    }
    find_axis(old, new, stride, rect, Axis::Horizontal)
}

fn find_axis(old: &[u8], new: &[u8], stride: usize, rect: Rect, axis: Axis) -> Option<Copy> {
    let (x0, y0) = (usize::from(rect.x), usize::from(rect.y));
    let (w, h) = (usize::from(rect.width), usize::from(rect.height));
    // `lines` are the rows (or columns) content slides along; `across` is
    // how long each is.
    let (lines, across) = match axis {
        Axis::Vertical => (h, w),
        Axis::Horizontal => (w, h),
    };
    if across < MIN_WIDTH || lines < MIN_ROWS * 2 {
        return None;
    }
    let old_hash: Vec<u64> = (0..lines)
        .map(|i| line_hash(old, stride, rect, axis, i))
        .collect();
    let new_hash: Vec<u64> = (0..lines)
        .map(|i| line_hash(new, stride, rect, axis, i))
        .collect();
    // The shift with the most matching lines. `shift > 0`: content moved
    // towards higher numbers, so new line i equals old line i - shift.
    let mut best: Option<(usize, isize)> = None;
    for shift in 1..=MAX_SHIFT.min(lines - MIN_ROWS) {
        for sign in [1isize, -1] {
            let d = shift as isize * sign;
            let mut matches = 0usize;
            for i in 0..lines {
                let Ok(src) = usize::try_from(i as isize - d) else {
                    continue;
                };
                if src < lines && new_hash[i] == old_hash[src] {
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
    // The longest run of consecutive lines that match at that shift, checked
    // against the real pixels (hashes can collide).
    let mut run = (0usize, 0usize); // (start, length)
    let mut start = None;
    for i in 0..=lines {
        let ok = i < lines
            && usize::try_from(i as isize - d).is_ok_and(|src| {
                src < lines
                    && new_hash[i] == old_hash[src]
                    && lines_equal(old, new, stride, rect, axis, src, i)
            });
        match (ok, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s > run.1 {
                    run = (s, i - s);
                }
                start = None;
            }
            _ => {}
        }
    }
    if run.1 < MIN_ROWS {
        return None;
    }
    // Blank lines match everything: ask for some variety.
    let distinct = {
        let mut seen: Vec<u64> = new_hash[run.0..run.0 + run.1].to_vec();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    };
    if distinct < MIN_DISTINCT_ROWS {
        return None;
    }
    let first = usize::try_from(run.0 as isize - d).ok()?;
    Some(match axis {
        Axis::Vertical => Copy {
            src_x: rect.x,
            src_y: u16::try_from(y0 + first).ok()?,
            dst: Rect {
                x: rect.x,
                y: u16::try_from(y0 + run.0).ok()?,
                width: rect.width,
                height: u16::try_from(run.1).ok()?,
            },
        },
        Axis::Horizontal => Copy {
            src_x: u16::try_from(x0 + first).ok()?,
            src_y: rect.y,
            dst: Rect {
                x: u16::try_from(x0 + run.0).ok()?,
                y: rect.y,
                width: u16::try_from(run.1).ok()?,
                height: rect.height,
            },
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

    /// A picture whose every column is different (a view scrolled sideways).
    fn strip(first_col: usize, cols: usize, rows: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                let v = (first_col + c) * 31 + r * 7;
                out.extend_from_slice(&[v as u8, (v >> 8) as u8 ^ 0x5A, (v / 3) as u8, 255]);
            }
        }
        out
    }

    #[test]
    fn sideways_scrolls_are_found_with_their_direction_and_distance() {
        let (cols, rows) = (200usize, 120usize);
        let rect = Rect {
            x: 0,
            y: 0,
            width: cols as u16,
            height: rows as u16,
        };
        // The view scrolled left by 40 columns: new column c shows old column c + 40.
        let old = strip(0, cols, rows);
        let new = strip(40, cols, rows);
        let c = find_copy_sideways(&old, &new, cols * 4, rect).expect("a sideways scroll");
        assert_eq!((c.src_x, c.src_y), (40, 0));
        assert_eq!(
            c.dst,
            Rect {
                x: 0,
                y: 0,
                width: (cols - 40) as u16,
                height: rows as u16
            }
        );
        // Scrolled right by 25 columns.
        let (old, new) = (strip(25, cols, rows), strip(0, cols, rows));
        let c = find_copy_sideways(&old, &new, cols * 4, rect).expect("a sideways scroll");
        assert_eq!((c.src_x, c.dst.x), (0, 25));
        assert_eq!(c.dst.width, (cols - 25) as u16);
        // A vertical scroll is not a sideways one, and the other way round.
        assert_eq!(find_copy(&old, &new, cols * 4, rect), None);
        assert_eq!(
            find_copy_sideways(&old, &old, cols * 4, rect),
            None,
            "nothing changed"
        );
    }

    #[test]
    fn a_sideways_scroll_inside_a_larger_picture_uses_the_pictures_coordinates() {
        // A 300 x 150 picture; only the 200 x 100 block at (30, 20) scrolled.
        let (pw, ph) = (300usize, 150usize);
        let mut old = vec![255u8; pw * ph * 4];
        let mut new = old.clone();
        let (a, b) = (strip(0, 200, 100), strip(40, 200, 100));
        for r in 0..100 {
            for c in 0..200 {
                let (src, dst) = ((r * 200 + c) * 4, ((20 + r) * pw + 30 + c) * 4);
                old[dst..dst + 4].copy_from_slice(&a[src..src + 4]);
                new[dst..dst + 4].copy_from_slice(&b[src..src + 4]);
            }
        }
        let rect = Rect {
            x: 30,
            y: 20,
            width: 200,
            height: 100,
        };
        let c = find_copy_sideways(&old, &new, pw * 4, rect).expect("a sideways scroll");
        assert_eq!((c.src_x, c.src_y), (70, 20));
        assert_eq!(
            c.dst,
            Rect {
                x: 30,
                y: 20,
                width: 160,
                height: 100
            }
        );
    }
}
