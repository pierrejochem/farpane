//! Several displays as one picture.
//!
//! macOS places displays in one coordinate space of *points* (a Retina
//! display has two pixels per point) with the main display's top left at the
//! origin and others anywhere around it, so coordinates can be negative. The
//! shared picture covers the bounding box of all displays at the finest
//! scale among them, like the Windows backend's virtual desktop; areas that
//! belong to no display stay black.

use rfb_proto::types::Rect;

/// One display as macOS reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayFrame {
    /// Top left and size in points.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Pixels per point (1 or 2, sometimes 3).
    pub scale: f64,
}

/// How displays are laid out in the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// The picture's top left, in points.
    origin: (f64, f64),
    /// Pixels per point of the picture.
    scale: f64,
    width: u16,
    height: u16,
}

impl Layout {
    /// The layout covering `displays`. `None` when there are none, a size is
    /// not a positive finite number, or the picture would not fit RFB's 16-bit sizes.
    #[must_use]
    pub fn from_displays(displays: &[DisplayFrame]) -> Option<Self> {
        let first = displays.first()?;
        let mut min = (first.x, first.y);
        let mut max = (first.x + first.width, first.y + first.height);
        let mut scale: f64 = 1.0;
        for d in displays {
            let ok = [d.x, d.y, d.width, d.height, d.scale]
                .iter()
                .all(|v| v.is_finite())
                && d.width > 0.0
                && d.height > 0.0
                && d.scale >= 1.0;
            if !ok {
                return None;
            }
            min = (min.0.min(d.x), min.1.min(d.y));
            max = (max.0.max(d.x + d.width), max.1.max(d.y + d.height));
            scale = scale.max(d.scale);
        }
        let width = ((max.0 - min.0) * scale).ceil();
        let height = ((max.1 - min.1) * scale).ceil();
        if !(1.0..=65535.0).contains(&width) || !(1.0..=65535.0).contains(&height) {
            return None;
        }
        // Checked to be in range above.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(Self {
            origin: min,
            scale,
            width: width as u16,
            height: height as u16,
        })
    }

    /// Size of the picture in pixels.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Pixels per point of the picture.
    #[must_use]
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// The global screen point (in points) under picture pixel (`x`, `y`);
    /// the centre of the pixel, which is what a pointer event should use.
    #[must_use]
    pub fn to_global(&self, x: u16, y: u16) -> (f64, f64) {
        (
            self.origin.0 + (f64::from(x) + 0.5) / self.scale,
            self.origin.1 + (f64::from(y) + 0.5) / self.scale,
        )
    }

    /// Where `display` sits in the picture, in pixels, clipped to it.
    #[must_use]
    pub fn display_rect(&self, display: &DisplayFrame) -> Rect {
        let clip = |v: f64, max: u16| {
            let v = v.round().clamp(0.0, f64::from(max));
            // Clamped into u16 range.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let v = v as u16;
            v
        };
        let x0 = clip((display.x - self.origin.0) * self.scale, self.width);
        let y0 = clip((display.y - self.origin.1) * self.scale, self.height);
        let x1 = clip(
            (display.x + display.width - self.origin.0) * self.scale,
            self.width,
        );
        let y1 = clip(
            (display.y + display.height - self.origin.1) * self.scale,
            self.height,
        );
        Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x: f64, y: f64, w: f64, h: f64, scale: f64) -> DisplayFrame {
        DisplayFrame {
            x,
            y,
            width: w,
            height: h,
            scale,
        }
    }

    #[test]
    fn one_retina_display_is_its_pixel_size() {
        let l = Layout::from_displays(&[d(0.0, 0.0, 1512.0, 982.0, 2.0)]).unwrap();
        assert_eq!(l.size(), (3024, 1964));
        // Pixel (0,0) is the centre of the first pixel: a quarter point in.
        assert_eq!(l.to_global(0, 0), (0.25, 0.25));
        assert_eq!(l.to_global(3023, 1963), (1511.75, 981.75));
    }

    #[test]
    fn displays_around_the_main_one_with_negative_coordinates() {
        // An external 1x display to the left, above the main one's top.
        let main = d(0.0, 0.0, 1440.0, 900.0, 2.0);
        let left = d(-1920.0, -180.0, 1920.0, 1080.0, 1.0);
        let l = Layout::from_displays(&[main, left]).unwrap();
        // Bounding box 3360 x 1080 points at scale 2.
        assert_eq!(l.size(), (6720, 2160));
        assert_eq!(l.scale(), 2.0);
        assert_eq!(
            l.display_rect(&left),
            Rect {
                x: 0,
                y: 0,
                width: 3840,
                height: 2160
            }
        );
        assert_eq!(
            l.display_rect(&main),
            Rect {
                x: 3840,
                y: 360,
                width: 2880,
                height: 1800
            }
        );
        // A pixel in the left display maps back into negative global points.
        let (gx, gy) = l.to_global(0, 0);
        assert!((gx + 1920.0 - 0.25).abs() < 1e-9 && (gy + 180.0 - 0.25).abs() < 1e-9);
    }

    #[test]
    fn unusable_input_gives_none() {
        assert!(Layout::from_displays(&[]).is_none());
        assert!(Layout::from_displays(&[d(0.0, 0.0, 0.0, 100.0, 1.0)]).is_none());
        assert!(Layout::from_displays(&[d(f64::NAN, 0.0, 10.0, 10.0, 1.0)]).is_none());
        assert!(Layout::from_displays(&[d(0.0, 0.0, 10.0, 10.0, 0.5)]).is_none());
        // Wider than RFB can address.
        assert!(Layout::from_displays(&[d(0.0, 0.0, 40000.0, 100.0, 2.0)]).is_none());
    }
}
