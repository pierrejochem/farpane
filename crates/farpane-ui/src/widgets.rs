//! Small custom widgets. They read colours from the caller (design tokens),
//! never hard-code them.

use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2};

/// Width of the sparkline stroke in points.
const LINE_WIDTH: f32 = 1.5;

/// The polyline for `values` (oldest first) inside `rect`.
///
/// Time runs left to right with the newest value at the right edge: `capacity`
/// is how many samples fill the whole width, so a series that is still filling
/// up grows in from the right instead of stretching. Values are scaled so that
/// `max` reaches the top (or the largest value, if `max` is `None`); negative
/// and non-finite values count as zero.
#[must_use]
pub fn sparkline_points(
    values: &[f32],
    rect: Rect,
    capacity: usize,
    max: Option<f32>,
) -> Vec<Pos2> {
    let clean = |v: f32| if v.is_finite() && v > 0.0 { v } else { 0.0 };
    let top = max.map_or_else(
        || values.iter().copied().map(clean).fold(0.0, f32::max),
        clean,
    );
    // Window to the newest `capacity` samples.
    let values = &values[values.len().saturating_sub(capacity)..];
    let slots = capacity.max(values.len()).max(2);
    #[allow(clippy::cast_precision_loss)] // sample counts are small
    let step = rect.width() / (slots - 1) as f32;
    values
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let from_right = values.len() - 1 - i;
            #[allow(clippy::cast_precision_loss)]
            let x = rect.right() - from_right as f32 * step;
            let level = if top > 0.0 {
                (clean(v) / top).min(1.0)
            } else {
                0.0
            };
            pos2(x, rect.bottom() - level * rect.height())
        })
        .collect()
}

/// Draw filled line series (oldest first) that share one vertical scale, with
/// faint grid lines, as in the dashboard's traffic chart.
///
/// Each series is `(values, colour)`; `capacity` is how many samples fill the
/// width. The scale tops out at the largest value over all series (at least
/// `min_top`, so an idle chart stays flat instead of magnifying noise).
pub fn area_chart(
    ui: &mut Ui,
    size: Vec2,
    series: &[(&[f32], Color32)],
    capacity: usize,
    min_top: f32,
    grid: Color32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let top = series
        .iter()
        .flat_map(|(v, _)| v.iter().copied())
        .filter(|v| v.is_finite())
        .fold(min_top.max(f32::MIN_POSITIVE), f32::max);
    for i in 1..4_u8 {
        let y = rect.top() + rect.height() * f32::from(i) / 4.0;
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0_f32, grid),
        );
    }
    let inner = rect.shrink2(Vec2::new(0.0, LINE_WIDTH));
    for &(values, colour) in series {
        let points = sparkline_points(values, inner, capacity, Some(top));
        if points.len() < 2 {
            continue;
        }
        let mut mesh = egui::Mesh::default();
        let (strong, faint) = (colour.gamma_multiply(0.38), colour.gamma_multiply(0.02));
        for p in &points {
            mesh.colored_vertex(*p, strong);
            mesh.colored_vertex(pos2(p.x, rect.bottom()), faint);
        }
        for i in 0..points.len() - 1 {
            let a = u32::try_from(i * 2).unwrap_or(0);
            mesh.add_triangle(a, a + 1, a + 2);
            mesh.add_triangle(a + 1, a + 3, a + 2);
        }
        painter.add(egui::Shape::mesh(mesh));
        painter.add(egui::Shape::line(points, Stroke::new(2.0_f32, colour)));
    }
    response
}

/// Draw a sparkline in a `size` box and return its response.
pub fn sparkline(
    ui: &mut Ui,
    values: &[f32],
    size: Vec2,
    capacity: usize,
    max: Option<f32>,
    colour: Color32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    // Keep the stroke inside the box.
    let inner = rect.shrink(LINE_WIDTH);
    let points = sparkline_points(values, inner, capacity, max);
    let painter = ui.painter_at(rect);
    painter.line_segment(
        [
            pos2(rect.left(), rect.bottom() - 0.5),
            pos2(rect.right(), rect.bottom() - 0.5),
        ],
        Stroke::new(1.0_f32, colour.gamma_multiply(0.25)),
    );
    if points.len() >= 2 {
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(LINE_WIDTH, colour),
        ));
    }
    if let Some(&last) = points.last() {
        painter.circle_filled(last, 2.5, colour);
    }
    response
}

/// Fill of an inset control (segmented control, bars).
#[must_use]
pub fn inset(tokens: &crate::theme::Tokens) -> egui::Color32 {
    if tokens.dark {
        egui::Color32::from_white_alpha(13)
    } else {
        egui::Color32::from_black_alpha(10)
    }
}

/// Fill of the chosen segment or the current page.
#[must_use]
pub fn selected(tokens: &crate::theme::Tokens) -> egui::Color32 {
    if tokens.dark {
        egui::Color32::from_white_alpha(34)
    } else {
        egui::Color32::WHITE
    }
}

/// A small pill with `text` in the colour `tone`.
pub fn badge(ui: &mut Ui, text: &str, tone: egui::Color32) {
    egui::Frame::NONE
        .fill(tone.gamma_multiply(0.13))
        .stroke(egui::Stroke::new(1.0_f32, tone.gamma_multiply(0.38)))
        .corner_radius(crate::style::radius(99.0))
        .inner_margin(egui::Margin::symmetric(9, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(tone).size(11.5).strong());
        });
}

/// The brand tile: receding panes on the accent colour.
pub fn brand_mark(ui: &mut Ui, tokens: &crate::theme::Tokens, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    // The receding panes of docs/brand/farpane-mark.svg, on its own grid:
    // x 248..796, y 240..792 of the 1024 view box, drawn into `rect`.
    let scale = size / 556.0;
    let at = |x: f32, y: f32| rect.min + egui::vec2((x - 240.0) * scale, (y - 240.0) * scale);
    let accent = egui::Color32::from(tokens.accent);
    // (x, y, side, corner radius, stroke width, opacity)
    for (x, y, side, r, w, a) in [
        (248.0_f32, 272.0_f32, 520.0_f32, 114.0_f32, 44.0_f32, 97_u8),
        (381.0, 265.0, 390.0, 86.0, 36.0, 158),
        (511.0, 255.0, 270.0, 60.0, 30.0, 219),
    ] {
        painter.rect_stroke(
            egui::Rect::from_min_max(at(x, y), at(x + side, y + side)),
            crate::style::radius(r * scale),
            egui::Stroke::new(w * scale, accent.gamma_multiply(f32::from(a) / 255.0)),
            egui::StrokeKind::Middle,
        );
    }
    painter.rect_filled(
        egui::Rect::from_min_max(at(636.0, 240.0), at(796.0, 400.0)),
        crate::style::radius(36.0 * scale),
        egui::Color32::from_rgb(0xFF, 0xB5, 0x9C),
    );
}

/// A row of the side bar. Returns whether it was clicked.
pub fn nav_item(
    ui: &mut Ui,
    tokens: &crate::theme::Tokens,
    active: bool,
    icon: crate::icons::Icon,
    title: &str,
    count: Option<usize>,
) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 42.0), egui::Sense::click());
    let label = count.map_or_else(|| title.to_owned(), |n| format!("{title} ({n})"));
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
    let painter = ui.painter();
    if active || response.hovered() {
        painter.rect(
            rect,
            crate::style::radius(12.0),
            if active {
                selected(tokens)
            } else {
                inset(tokens)
            },
            egui::Stroke::new(
                1.0_f32,
                if active {
                    egui::Color32::from(tokens.line)
                } else {
                    egui::Color32::TRANSPARENT
                },
            ),
            egui::StrokeKind::Inside,
        );
    }
    let icon_colour = if active {
        tokens.accent_text
    } else {
        tokens.muted
    };
    crate::icons::paint(
        painter,
        rect.left_center() + egui::vec2(21.0, 0.0),
        18.0,
        icon,
        egui::Color32::from(icon_colour),
    );
    let colour = if active { tokens.text } else { tokens.muted };
    painter.text(
        rect.left_center() + egui::vec2(42.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(14.0),
        egui::Color32::from(colour),
    );
    if let Some(n) = count {
        painter.text(
            rect.right_center() - egui::vec2(14.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            n.to_string(),
            egui::FontId::proportional(12.0),
            egui::Color32::from(tokens.muted),
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // the expected values are exact by construction
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), Vec2::new(100.0, 40.0))
    }

    #[test]
    fn empty_and_single_series() {
        assert_eq!(sparkline_points(&[], rect(), 11, None), []);
        // One sample sits at the right edge, at its height.
        let p = sparkline_points(&[5.0], rect(), 11, Some(10.0));
        assert_eq!(p, [pos2(110.0, 40.0)]);
    }

    #[test]
    fn a_full_series_spans_the_width_with_the_newest_at_the_right() {
        let values: Vec<f32> = (0..11_u8).map(f32::from).collect();
        let p = sparkline_points(&values, rect(), 11, None);
        assert_eq!(p.len(), 11);
        assert_eq!(p[0], pos2(10.0, 60.0), "oldest: left, zero is the bottom");
        assert_eq!(
            p[10],
            pos2(110.0, 20.0),
            "newest: right, the maximum is the top"
        );
        assert!(p.windows(2).all(|w| w[1].x > w[0].x));
        assert!((p[5].x - 60.0).abs() < 1e-4 && (p[5].y - 40.0).abs() < 1e-4);
    }

    #[test]
    fn a_short_series_grows_in_from_the_right() {
        let p = sparkline_points(&[1.0, 2.0, 3.0], rect(), 11, Some(3.0));
        assert_eq!(p.len(), 3);
        assert_eq!(p[2].x, 110.0);
        assert!(
            (p[1].x - 100.0).abs() < 1e-4,
            "one step of 10 points to the left"
        );
        assert!((p[0].x - 90.0).abs() < 1e-4);
    }

    #[test]
    fn a_long_series_shows_only_the_newest_samples() {
        let values: Vec<f32> = (0..100_u8).map(f32::from).collect();
        let p = sparkline_points(&values, rect(), 11, Some(99.0));
        assert_eq!(p.len(), 11);
        assert_eq!(p[10].y, 20.0, "99 is the top");
        assert!(p[0].y > p[10].y, "the window starts at 89");
    }

    #[test]
    fn bad_values_count_as_zero_and_a_fixed_max_clamps() {
        let p = sparkline_points(
            &[f32::NAN, -5.0, f32::INFINITY, 50.0],
            rect(),
            4,
            Some(10.0),
        );
        assert!(
            p.iter()
                .all(|q| q.y.is_finite() && (20.0..=60.0).contains(&q.y))
        );
        assert_eq!(p[0].y, 60.0);
        assert_eq!(p[1].y, 60.0);
        assert_eq!(p[2].y, 60.0, "infinity is not drawn off the chart");
        assert_eq!(p[3].y, 20.0, "above the maximum stays at the top");
        // All zero with automatic scaling: a flat line at the bottom, no division by zero.
        let flat = sparkline_points(&[0.0, 0.0, 0.0], rect(), 3, None);
        assert!(flat.iter().all(|q| q.y == 60.0));
    }
}
