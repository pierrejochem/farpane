//! Line icons drawn as egui shapes, after the 24 x 24 stroke icons of the
//! mockups (stroke 2, round caps). egui has no SVG renderer, so each icon is a
//! handful of rectangles, circles and polylines on that grid.

use egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind, vec2};

/// An icon of the side bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Overview,
    Clients,
    Users,
    Security,
    Log,
    Appearance,
    Displays,
}

/// Points of an arc around `(cx, cy)` from `from` to `to` degrees (0 is
/// right, 90 is down, as on screen).
fn arc(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    (0..=16_u8)
        .map(|i| {
            let a = (from + (to - from) * f32::from(i) / 16.0).to_radians();
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Draw `icon` in a `size` square centred on `centre`.
pub fn paint(painter: &Painter, centre: Pos2, size: f32, icon: Icon, colour: Color32) {
    let scale = size / 24.0;
    let origin = centre - vec2(size, size) / 2.0;
    let at = |x: f32, y: f32| origin + vec2(x, y) * scale;
    let stroke = Stroke::new((2.0 * scale).max(1.0), colour);
    let rect = |x: f32, y: f32, w: f32, h: f32, r: f32| {
        painter.rect_stroke(
            Rect::from_min_size(at(x, y), vec2(w, h) * scale),
            r * scale,
            stroke,
            StrokeKind::Middle,
        );
    };
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Overview => {
            rect(3.0, 3.0, 7.0, 9.0, 1.5);
            rect(14.0, 3.0, 7.0, 5.0, 1.5);
            rect(14.0, 12.0, 7.0, 9.0, 1.5);
            rect(3.0, 16.0, 7.0, 5.0, 1.5);
        }
        Icon::Clients => {
            painter.circle_stroke(at(9.0, 8.0), 3.5 * scale, stroke);
            line(&arc(9.0, 20.0, 6.5, 180.0, 360.0));
            line(&arc(16.5, 8.0, 3.5, -90.0, 90.0));
            line(&arc(18.5, 20.0, 4.0, 200.0, 360.0));
        }
        Icon::Users => {
            painter.circle_stroke(at(12.0, 8.0), 4.0 * scale, stroke);
            line(&arc(12.0, 21.0, 8.0, 180.0, 360.0));
        }
        Icon::Security => line(&[
            (12.0, 3.0),
            (20.0, 6.0),
            (20.0, 12.0),
            (16.5, 18.0),
            (12.0, 21.0),
            (7.5, 18.0),
            (4.0, 12.0),
            (4.0, 6.0),
            (12.0, 3.0),
        ]),
        Icon::Log => {
            line(&[(8.0, 6.0), (20.0, 6.0)]);
            line(&[(8.0, 12.0), (20.0, 12.0)]);
            line(&[(8.0, 18.0), (20.0, 18.0)]);
            for y in [6.0, 12.0, 18.0] {
                painter.circle_filled(at(4.0, y), 1.1 * scale, colour);
            }
        }
        Icon::Appearance => {
            painter.circle_stroke(at(12.0, 12.0), 9.0 * scale, stroke);
            // The left half filled.
            let half = arc(12.0, 12.0, 9.0, 90.0, 270.0);
            painter.add(egui::Shape::convex_polygon(
                half.iter().map(|&(x, y)| at(x, y)).collect(),
                colour,
                Stroke::NONE,
            ));
        }
        Icon::Displays => {
            rect(2.0, 4.0, 13.0, 10.0, 1.5);
            rect(17.0, 7.0, 5.0, 10.0, 1.0);
            line(&[(6.0, 20.0), (11.0, 20.0)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn every_icon_draws_without_panicking() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            for icon in [
                Icon::Overview,
                Icon::Clients,
                Icon::Users,
                Icon::Security,
                Icon::Log,
                Icon::Appearance,
                Icon::Displays,
            ] {
                paint(&painter, pos2(50.0, 50.0), 18.0, icon, Color32::WHITE);
            }
        });
    }

    #[test]
    fn arcs_start_and_end_where_asked() {
        let points = arc(0.0, 0.0, 10.0, 0.0, 180.0);
        assert!((points[0].0 - 10.0).abs() < 1e-4);
        let last = points.last().unwrap();
        assert!((last.0 + 10.0).abs() < 1e-4 && last.1.abs() < 1e-3);
    }
}
