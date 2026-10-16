//! When the keyboard should be captured, and the release chord.
//!
//! Capturing means system shortcuts (Alt+Tab and friends) go to the remote
//! machine instead of the local desktop. The rules live here as plain
//! functions so they can be tested without a window or a compositor.

use egui::Modifiers;
use farpane_grab::{GrabState, KeyboardGrab};

/// What the window needs from a keyboard grabber. [`KeyboardGrab`] is the
/// real one; tests use a recording fake.
pub trait Grabber {
    /// Ask for the capture or give the keyboard back; repeating is harmless.
    fn set_wanted(&mut self, wanted: bool);
    /// Process news and return the current state.
    fn poll(&mut self) -> GrabState;
    /// Which windowing system backend this is.
    fn backend(&self) -> &'static str;
}

impl Grabber for KeyboardGrab {
    fn set_wanted(&mut self, wanted: bool) {
        KeyboardGrab::set_wanted(self, wanted);
    }

    fn poll(&mut self) -> GrabState {
        KeyboardGrab::poll(self)
    }

    fn backend(&self) -> &'static str {
        self.backend_name()
    }
}

/// The user's choice for keyboard capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CapturePref {
    /// Capture in full screen only (the design default).
    #[default]
    Auto,
    /// Capture whenever the window has focus.
    Always,
    Never,
}

impl CapturePref {
    /// The name stored in the settings file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    /// An unknown name from a newer version means the default.
    #[must_use]
    pub fn from_key(key: &str) -> Self {
        match key {
            "always" => Self::Always,
            "never" => Self::Never,
            _ => Self::Auto,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Full screen only",
            Self::Always => "Always",
            Self::Never => "Never",
        }
    }
}

/// What the capture decision depends on.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)] // independent facts, not a state machine
pub struct Situation {
    pub connected: bool,
    pub window_focused: bool,
    pub fullscreen: bool,
    pub pref: CapturePref,
    /// The user used the release chord and has not asked for capture again.
    pub released_by_user: bool,
}

/// Whether the keyboard should be captured right now.
#[must_use]
pub fn should_capture(s: Situation) -> bool {
    let allowed = match s.pref {
        CapturePref::Never => false,
        CapturePref::Always => true,
        CapturePref::Auto => s.fullscreen,
    };
    allowed && s.connected && s.window_focused && !s.released_by_user
}

/// Detects the release chord: Ctrl, Alt and Shift pressed together and then
/// let go without any other key in between. Firing on release means a
/// shortcut that *uses* the three modifiers (such as Ctrl+Alt+Shift+S) never
/// triggers it, and the remote machine still sees the modifiers go down and up.
#[derive(Debug, Default)]
pub struct ChordTracker {
    /// All three are down and nothing else was pressed meanwhile.
    armed: bool,
    /// Another key was pressed while all three were down.
    spoiled: bool,
}

impl ChordTracker {
    /// Feed one frame. `other_key` is true if a non-modifier key was pressed
    /// in the frame. Returns true on the frame the chord is completed.
    pub fn update(&mut self, mods: Modifiers, other_key: bool) -> bool {
        if mods.ctrl && mods.alt && mods.shift {
            if other_key {
                self.spoiled = true;
                self.armed = false;
            } else if !self.spoiled {
                self.armed = true;
            }
            return false;
        }
        let fired = self.armed;
        *self = Self::default();
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;
    const ALL: Modifiers = Modifiers {
        ctrl: true,
        alt: true,
        shift: true,
        ..Modifiers::NONE
    };
    const CTRL_ALT: Modifiers = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::NONE
    };

    fn situation() -> Situation {
        Situation {
            connected: true,
            window_focused: true,
            fullscreen: true,
            pref: CapturePref::Auto,
            released_by_user: false,
        }
    }

    #[test]
    fn auto_captures_in_full_screen_only() {
        assert!(should_capture(situation()));
        assert!(!should_capture(Situation {
            fullscreen: false,
            ..situation()
        }));
    }

    #[test]
    fn always_and_never_ignore_full_screen() {
        let windowed = Situation {
            fullscreen: false,
            ..situation()
        };
        assert!(should_capture(Situation {
            pref: CapturePref::Always,
            ..windowed
        }));
        assert!(!should_capture(Situation {
            pref: CapturePref::Never,
            ..situation()
        }));
    }

    #[test]
    fn nothing_is_captured_without_a_connection_focus_or_after_a_release() {
        assert!(!should_capture(Situation {
            connected: false,
            ..situation()
        }));
        assert!(!should_capture(Situation {
            window_focused: false,
            ..situation()
        }));
        assert!(!should_capture(Situation {
            released_by_user: true,
            ..situation()
        }));
        let always = Situation {
            pref: CapturePref::Always,
            ..situation()
        };
        assert!(!should_capture(Situation {
            connected: false,
            ..always
        }));
        assert!(!should_capture(Situation {
            window_focused: false,
            ..always
        }));
        assert!(!should_capture(Situation {
            released_by_user: true,
            ..always
        }));
    }

    #[test]
    fn preferences_round_trip_and_unknown_names_mean_auto() {
        for p in [CapturePref::Auto, CapturePref::Always, CapturePref::Never] {
            assert_eq!(CapturePref::from_key(p.key()), p);
        }
        assert_eq!(CapturePref::from_key("sometimes"), CapturePref::Auto);
    }

    #[test]
    fn the_chord_fires_when_the_three_modifiers_are_let_go() {
        let mut c = ChordTracker::default();
        assert!(!c.update(NONE, false));
        assert!(!c.update(CTRL_ALT, false), "two of three is not it");
        assert!(!c.update(ALL, false), "nothing happens while they are held");
        assert!(!c.update(ALL, false));
        assert!(c.update(NONE, false), "released: fire");
        assert!(!c.update(NONE, false), "once");
    }

    #[test]
    fn a_key_in_between_spoils_it_so_shortcuts_using_the_modifiers_are_safe() {
        let mut c = ChordTracker::default();
        c.update(ALL, false);
        c.update(ALL, true); // Ctrl+Alt+Shift+S, say
        assert!(!c.update(ALL, false));
        assert!(!c.update(NONE, false), "spoiled: no fire on release");
        // And it is armed again for the next real chord.
        c.update(ALL, false);
        assert!(c.update(NONE, false));
    }

    #[test]
    fn letting_go_one_at_a_time_still_counts_once() {
        let mut c = ChordTracker::default();
        c.update(ALL, false);
        assert!(
            c.update(CTRL_ALT, false),
            "the first modifier released ends the chord"
        );
        assert!(!c.update(NONE, false));
    }

    #[test]
    fn a_key_pressed_in_the_same_frame_as_the_third_modifier_spoils_it() {
        let mut c = ChordTracker::default();
        assert!(!c.update(ALL, true));
        assert!(!c.update(NONE, false));
    }
}
