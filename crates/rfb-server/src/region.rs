//! A set of changed rectangles, kept small.
#![allow(clippy::many_single_char_names)] // rectangle arithmetic reads best with x, y, r

use rfb_proto::types::Rect;

/// Most rectangles kept before everything collapses into one bounding box.
const MAX_RECTS: usize = 16;

/// Pending changes of one client. Cheap to merge: overlapping rectangles
/// unite, and past 16 pieces the region becomes its bounding box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Region {
    rects: Vec<Rect>,
}

fn right(r: Rect) -> u32 {
    u32::from(r.x) + u32::from(r.width)
}

fn bottom(r: Rect) -> u32 {
    u32::from(r.y) + u32::from(r.height)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    u32::from(a.x) < right(b)
        && u32::from(b.x) < right(a)
        && u32::from(a.y) < bottom(b)
        && u32::from(b.y) < bottom(a)
}

pub(crate) fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    // Both edges fit a `u16` because both inputs fit.
    let r = right(a).max(right(b));
    let bt = bottom(a).max(bottom(b));
    Rect {
        x,
        y,
        width: u16::try_from(r - u32::from(x)).unwrap_or(u16::MAX),
        height: u16::try_from(bt - u32::from(y)).unwrap_or(u16::MAX),
    }
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    if !overlaps(a, b) {
        return None;
    }
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = right(a).min(right(b));
    let bt = bottom(a).min(bottom(b));
    Some(Rect {
        x,
        y,
        width: u16::try_from(r - u32::from(x)).unwrap_or(0),
        height: u16::try_from(bt - u32::from(y)).unwrap_or(0),
    })
}

/// `a` without `b`: up to four rectangles.
fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
    let Some(i) = intersection(a, b) else {
        return vec![a];
    };
    let mut out = Vec::with_capacity(4);
    if i.y > a.y {
        out.push(Rect {
            height: i.y - a.y,
            ..a
        });
    }
    if bottom(i) < bottom(a) {
        let y = u16::try_from(bottom(i)).unwrap_or(u16::MAX);
        out.push(Rect {
            y,
            height: u16::try_from(bottom(a) - bottom(i)).unwrap_or(0),
            ..a
        });
    }
    if i.x > a.x {
        out.push(Rect {
            y: i.y,
            width: i.x - a.x,
            height: i.height,
            ..a
        });
    }
    if right(i) < right(a) {
        out.push(Rect {
            x: u16::try_from(right(i)).unwrap_or(u16::MAX),
            y: i.y,
            width: u16::try_from(right(a) - right(i)).unwrap_or(0),
            height: i.height,
        });
    }
    out
}

impl Region {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// The pieces.
    #[must_use]
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// Add `rect` (empty rectangles are ignored).
    pub fn add(&mut self, rect: Rect) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let mut rect = rect;
        // Absorb every piece it overlaps, which may make it overlap more.
        while let Some(i) = self.rects.iter().position(|&r| overlaps(r, rect)) {
            rect = union(self.rects.swap_remove(i), rect);
        }
        self.rects.push(rect);
        if self.rects.len() > MAX_RECTS {
            let all = self.rects.iter().copied().reduce(union).unwrap_or(rect);
            self.rects.clear();
            self.rects.push(all);
        }
    }

    /// Remove and return the part of the region inside `area`.
    pub fn take_within(&mut self, area: Rect) -> Vec<Rect> {
        let mut taken = Vec::new();
        let mut kept = Vec::with_capacity(self.rects.len());
        for &r in &self.rects {
            if let Some(i) = intersection(r, area) {
                taken.push(i);
                kept.extend(subtract(r, area));
            } else {
                kept.push(r);
            }
        }
        self.rects = kept;
        taken
    }

    /// Whether any part lies inside `area`.
    #[must_use]
    pub fn touches(&self, area: Rect) -> bool {
        self.rects.iter().any(|&r| overlaps(r, area))
    }

    /// Drop everything outside a `width` x `height` picture.
    pub fn clip(&mut self, width: u16, height: u16) {
        let picture = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let clipped: Vec<Rect> = self
            .rects
            .iter()
            .filter_map(|&r| intersection(r, picture))
            .collect();
        self.rects = clipped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn area(region: &Region) -> u64 {
        region
            .rects()
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum()
    }

    #[test]
    fn overlapping_rectangles_unite_and_disjoint_ones_stay_apart() {
        let mut region = Region::new();
        region.add(r(0, 0, 10, 10));
        region.add(r(20, 20, 5, 5));
        assert_eq!(region.rects().len(), 2);
        region.add(r(5, 5, 20, 20)); // bridges both
        assert_eq!(region.rects(), [r(0, 0, 25, 25)]);
    }

    #[test]
    fn touching_edges_do_not_count_as_overlap() {
        let mut region = Region::new();
        region.add(r(0, 0, 10, 10));
        region.add(r(10, 0, 10, 10));
        assert_eq!(region.rects().len(), 2);
    }

    #[test]
    fn empty_rectangles_are_ignored() {
        let mut region = Region::new();
        region.add(r(3, 3, 0, 5));
        region.add(r(3, 3, 5, 0));
        assert!(region.is_empty());
    }

    #[test]
    fn too_many_pieces_collapse_but_nothing_is_lost() {
        let mut region = Region::new();
        let inputs: Vec<Rect> = (0..40).map(|i| r(i * 20, i * 20, 5, 5)).collect();
        for &input in &inputs {
            region.add(input);
            assert!(region.rects().len() <= MAX_RECTS);
        }
        // Every input is still covered by some piece.
        for input in inputs {
            assert!(
                region.rects().iter().any(|&p| union(p, input) == p),
                "{input:?} is lost"
            );
        }
    }

    #[test]
    fn taking_a_part_leaves_the_rest() {
        let mut region = Region::new();
        region.add(r(0, 0, 10, 10));
        let taken = region.take_within(r(2, 3, 4, 4));
        assert_eq!(taken, [r(2, 3, 4, 4)]);
        assert_eq!(area(&region), 100 - 16);
        assert!(!region.touches(r(2, 3, 4, 4)));
        assert!(region.touches(r(0, 0, 1, 1)));
        // Everything that is left plus what was taken is the original.
        let mut again = region.clone();
        for t in taken {
            again.add(t);
        }
        assert_eq!(area(&again), 100);
    }

    #[test]
    fn taking_outside_the_region_takes_nothing() {
        let mut region = Region::new();
        region.add(r(0, 0, 10, 10));
        assert_eq!(region.take_within(r(50, 50, 5, 5)), []);
        assert_eq!(area(&region), 100);
    }

    #[test]
    fn clipping_drops_what_left_the_picture() {
        let mut region = Region::new();
        region.add(r(0, 0, 100, 100));
        region.add(r(200, 200, 10, 10));
        region.clip(50, 60);
        assert_eq!(region.rects(), [r(0, 0, 50, 60)]);
    }
}
