//! Maps design tokens to egui styling. Widgets and screens read tokens through
//! this module and never hard-code colours.

use egui::{Color32, CornerRadius, Stroke, Visuals};

use crate::theme::{Rgba, Tokens};

impl From<Rgba> for Color32 {
    fn from(c: Rgba) -> Self {
        Self::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
    }
}

/// Width of borders and outlines in points.
const STROKE_WIDTH: f32 = 1.0;

/// Corner radius for panels and windows, clamped to what egui can store.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255
fn radius(px: f32) -> CornerRadius {
    CornerRadius::same(px.round().clamp(0.0, 255.0) as u8)
}

/// egui visuals for `tokens`.
#[must_use]
pub fn visuals(tokens: &Tokens) -> Visuals {
    let mut v = if tokens.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let text: Color32 = tokens.text.into();
    let card: Color32 = tokens.card.into();
    let line = Stroke::new(STROKE_WIDTH, Color32::from(tokens.line));
    let accent: Color32 = tokens.accent.into();
    // Widgets use a smaller radius than panels, as in the mockups.
    let widget_radius = radius(tokens.radius / 2.5);

    v.override_text_color = Some(text);
    v.panel_fill = tokens.bg.into();
    v.window_fill = card;
    v.window_stroke = line;
    v.window_corner_radius = radius(tokens.radius);
    v.menu_corner_radius = widget_radius;
    v.extreme_bg_color = tokens.bg.into();
    v.faint_bg_color = card;
    v.hyperlink_color = tokens.accent_text.into();
    v.warn_fg_color = tokens.amber.into();
    v.error_fg_color = tokens.red.into();
    v.selection.bg_fill = accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(STROKE_WIDTH, Color32::from(tokens.accent_text));

    let w = &mut v.widgets;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = widget_radius;
        state.fg_stroke = Stroke::new(STROKE_WIDTH, text);
    }
    w.noninteractive.bg_fill = card;
    w.noninteractive.weak_bg_fill = card;
    w.noninteractive.bg_stroke = line;
    w.inactive.bg_fill = card;
    w.inactive.weak_bg_fill = card;
    w.inactive.bg_stroke = line;
    w.hovered.bg_fill = card.gamma_multiply(1.3);
    w.hovered.weak_bg_fill = card.gamma_multiply(1.3);
    w.hovered.bg_stroke = Stroke::new(STROKE_WIDTH, Color32::from(tokens.accent_text));
    w.active.bg_fill = accent;
    w.active.weak_bg_fill = accent;
    w.active.bg_stroke = Stroke::new(STROKE_WIDTH, Color32::from(tokens.accent_text));
    w.open.bg_fill = card;
    w.open.weak_bg_fill = card;
    v
}

/// Apply `tokens` to `ctx`.
pub fn apply(ctx: &egui::Context, tokens: &Tokens) {
    ctx.set_visuals(visuals(tokens));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Accent, Theme};

    const THEMES: [Theme; 3] = [Theme::Glass, Theme::Slate, Theme::Light];
    const ACCENTS: [Accent; 6] = [
        Accent::ThemeDefault,
        Accent::Ember,
        Accent::Cyan,
        Accent::Violet,
        Accent::Green,
        Accent::Amber,
    ];

    #[test]
    fn tokens_reach_the_visuals() {
        for theme in THEMES {
            for accent in ACCENTS {
                let t = Tokens::new(theme, accent);
                let v = visuals(&t);
                assert_eq!(v.panel_fill, Color32::from(t.bg), "{theme:?}");
                assert_eq!(v.override_text_color, Some(Color32::from(t.text)));
                assert_eq!(v.widgets.active.bg_fill, Color32::from(t.accent));
                assert_eq!(v.dark_mode, t.dark, "{theme:?}");
                assert_eq!(v.window_corner_radius, radius(t.radius));
            }
        }
    }

    /// Straight-alpha colour `fg` over opaque `bg`, as linear-free sRGB bytes.
    fn over(fg: Rgba, bg: Rgba) -> [f64; 3] {
        let a = f64::from(fg.3) / 255.0;
        [
            f64::from(fg.0) * a + f64::from(bg.0) * (1.0 - a),
            f64::from(fg.1) * a + f64::from(bg.1) * (1.0 - a),
            f64::from(fg.2) * a + f64::from(bg.2) * (1.0 - a),
        ]
    }

    fn luminance(rgb: [f64; 3]) -> f64 {
        let lin = |c: f64| {
            let c = c / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
    }

    /// WCAG contrast ratio of `fg` over `bg`.
    fn contrast(fg: Rgba, bg: Rgba) -> f64 {
        let (a, b) = (luminance(over(fg, bg)), luminance(over(bg, bg)));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn text_colours_meet_wcag_aa_on_the_background() {
        for theme in THEMES {
            let t = Tokens::new(theme, Accent::ThemeDefault);
            for (name, c) in [
                ("text", t.text),
                ("muted", t.muted),
                ("accent_text", t.accent_text),
                ("green", t.green),
                ("blue", t.blue),
                ("amber", t.amber),
                ("red", t.red),
                ("violet", t.violet),
            ] {
                let ratio = contrast(c, t.bg);
                assert!(
                    ratio >= 4.5,
                    "{theme:?} {name}: {ratio:.2}:1 on the background"
                );
            }
        }
    }
}
