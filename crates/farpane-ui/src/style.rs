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
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255
pub fn radius(px: f32) -> CornerRadius {
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

/// Name of the heavier font family, for headings: `RichText::family`.
pub const SEMIBOLD: &str = "semibold";

const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const PLEX: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf");
const PLEX_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf");
const BACKDROP: &[u8] = include_bytes!("../assets/glass-backdrop.png");

fn semibold_name() -> egui::FontFamily {
    egui::FontFamily::Name(SEMIBOLD.into())
}

/// The text family for headings in the current font. egui installs new fonts
/// at the start of the next pass, so until then (the very first frame) this is
/// the ordinary family; asking for an unbound family would panic.
#[must_use]
pub fn semibold(ctx: &egui::Context) -> egui::FontFamily {
    let installed = ctx.data(|d| d.get_temp::<u64>(egui::Id::new("farpane-fonts-pass")));
    match installed {
        Some(pass) if pass < ctx.cumulative_pass_nr() => semibold_name(),
        _ => egui::FontFamily::Proportional,
    }
}

/// Install Inter (or IBM Plex Sans for `plex`) as the proportional font. The
/// built-in fonts stay behind them as fallback for glyphs they lack.
fn install_fonts(ctx: &egui::Context, plex: bool) {
    let (regular, bold) = if plex {
        (PLEX, PLEX_SEMIBOLD)
    } else {
        (INTER, INTER_SEMIBOLD)
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "farpane-regular".into(),
        std::sync::Arc::new(egui::FontData::from_static(regular)),
    );
    fonts.font_data.insert(
        "farpane-semibold".into(),
        std::sync::Arc::new(egui::FontData::from_static(bold)),
    );
    let fallback = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut proportional = vec!["farpane-regular".to_owned()];
    proportional.extend(fallback.iter().cloned());
    let mut heavy = vec!["farpane-semibold".to_owned()];
    heavy.extend(fallback);
    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);
    fonts.families.insert(semibold_name(), heavy);
    ctx.set_fonts(fonts);
}

/// Apply `tokens` to `ctx`: visuals and, when the theme changed, its font.
pub fn apply(ctx: &egui::Context, tokens: &Tokens) {
    ctx.set_visuals(visuals(tokens));
    let id = egui::Id::new("farpane-font-is-plex");
    if ctx.data(|d| d.get_temp::<bool>(id)) != Some(tokens.plex) {
        install_fonts(ctx, tokens.plex);
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            d.insert_temp(id, tokens.plex);
            // Only set the first time: the family stays bound on a switch.
            if d.get_temp::<u64>(egui::Id::new("farpane-fonts-pass"))
                .is_none()
            {
                d.insert_temp(egui::Id::new("farpane-fonts-pass"), pass);
            }
        });
    }
}

/// What panels should fill themselves with: nothing on the Glass theme, so
/// the backdrop shows through, else the background colour.
#[must_use]
pub fn page_fill(tokens: &Tokens) -> Color32 {
    if tokens.glass {
        Color32::TRANSPARENT
    } else {
        tokens.bg.into()
    }
}

/// Paint the Glass theme's backdrop over the whole window, below every
/// panel. Call it first in the frame; other themes paint nothing.
pub fn paint_backdrop(ctx: &egui::Context, tokens: &Tokens) {
    if !tokens.glass {
        return;
    }
    let id = egui::Id::new("farpane-backdrop");
    let texture = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id));
    let texture = texture.or_else(|| {
        let image = decode_backdrop()?;
        let handle = ctx.load_texture("farpane-backdrop", image, egui::TextureOptions::LINEAR);
        ctx.data_mut(|d| d.insert_temp(id, handle.clone()));
        Some(handle)
    });
    let Some(texture) = texture else {
        return;
    };
    let screen = ctx.screen_rect();
    // Cover the window, keeping the picture's proportions (SVG "slice").
    let size = texture.size_vec2();
    let scale = (screen.width() / size.x).max(screen.height() / size.y);
    let shown = egui::vec2(
        screen.width() / scale / size.x,
        screen.height() / scale / size.y,
    );
    let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), shown);
    ctx.layer_painter(egui::LayerId::background())
        .image(texture.id(), screen, uv, Color32::WHITE);
}

fn decode_backdrop() -> Option<egui::ColorImage> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(BACKDROP))
        .read_info()
        .ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return None,
    };
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [info.width as usize, info.height as usize],
        &rgba,
    ))
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
