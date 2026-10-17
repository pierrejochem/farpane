//! Themes and appearance options (Glass, Slate, Light, System).
#![allow(clippy::unreadable_literal)] // colours read as #RRGGBB

/// An sRGB colour with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    /// Opaque colour from `0xRRGGBB`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // masked to 8 bits first
    pub const fn hex(rgb: u32) -> Self {
        Self(
            ((rgb >> 16) & 0xFF) as u8,
            ((rgb >> 8) & 0xFF) as u8,
            (rgb & 0xFF) as u8,
            255,
        )
    }
}

/// A concrete theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Glass,
    Slate,
    Light,
}

/// What the user picked in Settings › Appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    /// Light when the OS is in light mode, Glass in dark mode.
    System,
    #[default]
    Glass,
    Slate,
    Light,
}

impl ThemePreference {
    /// Resolve against the OS appearance.
    #[must_use]
    pub fn resolve(self, os_is_dark: bool) -> Theme {
        match self {
            Self::System if os_is_dark => Theme::Glass,
            Self::System | Self::Light => Theme::Light,
            Self::Glass => Theme::Glass,
            Self::Slate => Theme::Slate,
        }
    }
}

/// Accent colour option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accent {
    #[default]
    ThemeDefault,
    Ember,
    Cyan,
    Violet,
    Green,
    Amber,
}

/// Semantic design tokens. Widgets read only these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub bg: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub line: Rgba,
    pub card: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub green: Rgba,
    pub blue: Rgba,
    pub amber: Rgba,
    pub red: Rgba,
    pub violet: Rgba,
    pub radius: f32,
    /// Dark surfaces (Glass, Slate) rather than light ones.
    pub dark: bool,
    /// Frosted-glass panels (translucent cards on the plain background).
    pub glass: bool,
    /// IBM Plex Sans instead of Inter (the Slate theme's typeface).
    pub plex: bool,
}

impl Tokens {
    /// Tokens for a theme with the user's accent applied.
    #[must_use]
    pub fn new(theme: Theme, accent: Accent) -> Self {
        let mut t = match theme {
            Theme::Glass => Self {
                bg: Rgba::hex(0x050505),
                text: Rgba::hex(0xF4F4F5),
                muted: Rgba(255, 255, 255, 158),
                line: Rgba(255, 255, 255, 23),
                card: Rgba(30, 30, 33, 176),
                accent: Rgba::hex(0xE8492F),
                accent_text: Rgba::hex(0xFF8A73),
                green: Rgba::hex(0x34D399),
                blue: Rgba::hex(0x38BDF8),
                amber: Rgba::hex(0xFBBF24),
                red: Rgba::hex(0xF87171),
                violet: Rgba::hex(0xA78BFA),
                radius: 18.0,
                dark: true,
                glass: true,
                plex: false,
            },
            Theme::Slate => Self {
                bg: Rgba::hex(0x0B1015),
                text: Rgba::hex(0xE6EDF3),
                muted: Rgba::hex(0x9AA7B5),
                line: Rgba::hex(0x1F2933),
                card: Rgba::hex(0x111821),
                accent: Rgba::hex(0x38BDF8),
                accent_text: Rgba::hex(0x7DD3FC),
                green: Rgba::hex(0x34D399),
                blue: Rgba::hex(0x38BDF8),
                amber: Rgba::hex(0xFBBF24),
                red: Rgba::hex(0xF87171),
                violet: Rgba::hex(0xA78BFA),
                radius: 14.0,
                dark: true,
                glass: false,
                plex: true,
            },
            Theme::Light => Self {
                bg: Rgba::hex(0xF3F4F6),
                text: Rgba::hex(0x14171C),
                muted: Rgba::hex(0x525B68),
                line: Rgba::hex(0xE2E5EA),
                card: Rgba::hex(0xFFFFFF),
                accent: Rgba::hex(0xD9432A),
                accent_text: Rgba::hex(0xB5361F),
                green: Rgba::hex(0x0F7B55),
                blue: Rgba::hex(0x0B69A3),
                amber: Rgba::hex(0x9A5B05),
                red: Rgba::hex(0xBE3A32),
                violet: Rgba::hex(0x6D4FD6),
                radius: 16.0,
                dark: false,
                glass: false,
                plex: false,
            },
        };
        let light = theme == Theme::Light;
        let pick = |dark: (u32, u32), light_v: (u32, u32)| {
            let (a, txt) = if light { light_v } else { dark };
            (Rgba::hex(a), Rgba::hex(txt))
        };
        let over = match accent {
            Accent::ThemeDefault => None,
            Accent::Ember => Some(pick((0xE8492F, 0xFF8A73), (0xD9432A, 0xB5361F))),
            Accent::Cyan => Some(pick((0x0EA5E9, 0x7DD3FC), (0x0284C7, 0x075985))),
            Accent::Violet => Some(pick((0x8B5CF6, 0xC4B5FD), (0x7C3AED, 0x6D28D9))),
            Accent::Green => Some(pick((0x10B981, 0x6EE7B7), (0x059669, 0x047857))),
            Accent::Amber => Some(pick((0xF59E0B, 0xFCD34D), (0xD97706, 0x92400E))),
        };
        if let Some((a, txt)) = over {
            t.accent = a;
            t.accent_text = txt;
        }
        t
    }

    /// Apply "Reduce transparency": solid panels, no blur pass.
    #[must_use]
    pub fn reduce_transparency(mut self) -> Self {
        if self.glass {
            self.glass = false;
            self.card = Rgba::hex(0x1B1B1E);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_os() {
        assert_eq!(ThemePreference::System.resolve(true), Theme::Glass);
        assert_eq!(ThemePreference::System.resolve(false), Theme::Light);
    }

    #[test]
    fn accent_overrides_theme_default() {
        let t = Tokens::new(Theme::Slate, Accent::Ember);
        assert_eq!(t.accent, Rgba::hex(0xE8492F));
    }
}
