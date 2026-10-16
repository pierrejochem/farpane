//! Mapping between the settings file's plain strings and the UI's types.

use farpane_ui::{Accent, ThemePreference};

pub const THEMES: [ThemePreference; 4] = [
    ThemePreference::Glass,
    ThemePreference::Slate,
    ThemePreference::Light,
    ThemePreference::System,
];

pub const ACCENTS: [Accent; 6] = [
    Accent::ThemeDefault,
    Accent::Ember,
    Accent::Cyan,
    Accent::Violet,
    Accent::Green,
    Accent::Amber,
];

/// Interface sizes in percent (design doc, section 16).
pub const SIZES: [u32; 3] = [90, 100, 115];

#[must_use]
pub fn theme_key(t: ThemePreference) -> &'static str {
    match t {
        ThemePreference::Glass => "glass",
        ThemePreference::Slate => "slate",
        ThemePreference::Light => "light",
        ThemePreference::System => "system",
    }
}

#[must_use]
pub fn theme_label(t: ThemePreference) -> &'static str {
    match t {
        ThemePreference::Glass => "Glass",
        ThemePreference::Slate => "Slate",
        ThemePreference::Light => "Light",
        ThemePreference::System => "System",
    }
}

/// An unknown name (from a newer version's file) gives the default theme.
#[must_use]
pub fn theme_from(key: &str) -> ThemePreference {
    THEMES
        .into_iter()
        .find(|t| theme_key(*t) == key)
        .unwrap_or_default()
}

#[must_use]
pub fn accent_key(a: Accent) -> &'static str {
    match a {
        Accent::ThemeDefault => "default",
        Accent::Ember => "ember",
        Accent::Cyan => "cyan",
        Accent::Violet => "violet",
        Accent::Green => "green",
        Accent::Amber => "amber",
    }
}

#[must_use]
pub fn accent_label(a: Accent) -> &'static str {
    match a {
        Accent::ThemeDefault => "Theme default",
        Accent::Ember => "Ember",
        Accent::Cyan => "Cyan",
        Accent::Violet => "Violet",
        Accent::Green => "Green",
        Accent::Amber => "Amber",
    }
}

#[must_use]
pub fn accent_from(key: &str) -> Accent {
    ACCENTS
        .into_iter()
        .find(|a| accent_key(*a) == key)
        .unwrap_or_default()
}

/// Snap a stored size to one of the offered sizes (anything else is 100).
#[must_use]
pub fn size_from(percent: u32) -> u32 {
    if SIZES.contains(&percent) {
        percent
    } else {
        100
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_option_round_trips_through_its_key() {
        for t in THEMES {
            assert_eq!(theme_from(theme_key(t)), t);
        }
        for a in ACCENTS {
            assert_eq!(accent_from(accent_key(a)), a);
        }
        for s in SIZES {
            assert_eq!(size_from(s), s);
        }
    }

    #[test]
    fn unknown_values_fall_back_to_defaults() {
        assert_eq!(theme_from("neon"), ThemePreference::default());
        assert_eq!(accent_from("plaid"), Accent::default());
        assert_eq!(size_from(250), 100);
        assert_eq!(size_from(0), 100);
    }
}
