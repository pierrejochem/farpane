//! Turns egui input events into RFB key and pointer events.
//!
//! Pure functions and small state machines, so they are testable without a
//! window. Keys use X11 keysyms, as the protocol wants.

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Vec2};

/// One key press or release to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyAction {
    pub keysym: u32,
    pub down: bool,
}

/// X11 keysyms used here (`keysymdef.h`).
pub mod keysym {
    pub const BACKSPACE: u32 = 0xFF08;
    pub const TAB: u32 = 0xFF09;
    pub const RETURN: u32 = 0xFF0D;
    pub const ESCAPE: u32 = 0xFF1B;
    pub const HOME: u32 = 0xFF50;
    pub const LEFT: u32 = 0xFF51;
    pub const UP: u32 = 0xFF52;
    pub const RIGHT: u32 = 0xFF53;
    pub const DOWN: u32 = 0xFF54;
    pub const PAGE_UP: u32 = 0xFF55;
    pub const PAGE_DOWN: u32 = 0xFF56;
    pub const END: u32 = 0xFF57;
    pub const INSERT: u32 = 0xFF63;
    pub const DELETE: u32 = 0xFFFF;
    pub const F1: u32 = 0xFFBE;
    pub const SHIFT_L: u32 = 0xFFE1;
    pub const CONTROL_L: u32 = 0xFFE3;
    pub const ALT_L: u32 = 0xFFE9;
    pub const SUPER_L: u32 = 0xFFEB;
}

/// The keysym for a typed character: Latin-1 maps to itself, everything else
/// to `0x0100_0000 + code point` (keysymdef.h, Unicode keysyms). Control
/// characters have no keysym.
#[must_use]
pub fn keysym_for_char(c: char) -> Option<u32> {
    let cp = u32::from(c);
    match cp {
        0x20..=0x7E | 0xA0..=0xFF => Some(cp),
        0x00..=0x1F | 0x7F..=0x9F => None,
        _ => Some(0x0100_0000 + cp),
    }
}

/// The keysym for a key that does not type text by itself, or that is used
/// with Ctrl/Alt (where the OS gives no text): navigation, editing, function
/// keys, and letters/digits/symbols as their unshifted character.
#[must_use]
pub fn keysym_for_key(key: Key) -> Option<u32> {
    let special = match key {
        Key::Backspace => keysym::BACKSPACE,
        Key::Tab => keysym::TAB,
        Key::Enter => keysym::RETURN,
        Key::Escape => keysym::ESCAPE,
        Key::Space => 0x20,
        Key::Delete => keysym::DELETE,
        Key::Insert => keysym::INSERT,
        Key::Home => keysym::HOME,
        Key::End => keysym::END,
        Key::PageUp => keysym::PAGE_UP,
        Key::PageDown => keysym::PAGE_DOWN,
        Key::ArrowLeft => keysym::LEFT,
        Key::ArrowUp => keysym::UP,
        Key::ArrowRight => keysym::RIGHT,
        Key::ArrowDown => keysym::DOWN,
        Key::F1 => keysym::F1,
        Key::F2 => keysym::F1 + 1,
        Key::F3 => keysym::F1 + 2,
        Key::F4 => keysym::F1 + 3,
        Key::F5 => keysym::F1 + 4,
        Key::F6 => keysym::F1 + 5,
        Key::F7 => keysym::F1 + 6,
        Key::F8 => keysym::F1 + 7,
        Key::F9 => keysym::F1 + 8,
        Key::F10 => keysym::F1 + 9,
        Key::F11 => keysym::F1 + 10,
        Key::F12 => keysym::F1 + 11,
        Key::Colon => u32::from(b':'),
        Key::Comma => u32::from(b','),
        Key::Backslash => u32::from(b'\\'),
        Key::Slash => u32::from(b'/'),
        Key::Pipe => u32::from(b'|'),
        Key::Questionmark => u32::from(b'?'),
        Key::Exclamationmark => u32::from(b'!'),
        Key::OpenBracket => u32::from(b'['),
        Key::CloseBracket => u32::from(b']'),
        Key::OpenCurlyBracket => u32::from(b'{'),
        Key::CloseCurlyBracket => u32::from(b'}'),
        Key::Backtick => u32::from(b'`'),
        Key::Minus => u32::from(b'-'),
        Key::Period => u32::from(b'.'),
        Key::Plus => u32::from(b'+'),
        Key::Equals => u32::from(b'='),
        Key::Semicolon => u32::from(b';'),
        Key::Quote => u32::from(b'\''),
        other => {
            // Letters and digits: egui names them by the character ("A", "5").
            // Servers expect the unshifted, lowercase keysym. (Symbols are
            // listed above: egui's display name for Minus is U+2212, not '-'.)
            let name = other.name();
            let mut chars = name.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => keysym_for_char(c.to_ascii_lowercase()),
                _ => None,
            };
        }
    };
    Some(special)
}

/// True for keys that produce text, which arrives as `Event::Text` and must
/// not be sent twice.
fn types_text(key: Key) -> bool {
    key == Key::Space || (keysym_for_key(key).is_some_and(|k| (0x20..=0x7E).contains(&k)))
}

/// Tracks which modifiers and keys the remote side believes are down, so they
/// can be released when focus is lost.
#[derive(Debug, Default)]
pub struct KeyTranslator {
    mods: Modifiers,
    held: Vec<u32>,
}

impl KeyTranslator {
    /// Translate one frame's events. `frame_mods` are the modifiers at the end
    /// of the frame.
    pub fn translate(&mut self, events: &[Event], frame_mods: Modifiers) -> Vec<KeyAction> {
        let mut out = Vec::new();
        for event in events {
            match event {
                Event::Text(text) => {
                    for keysym in text.chars().filter_map(keysym_for_char) {
                        out.push(KeyAction { keysym, down: true });
                        out.push(KeyAction {
                            keysym,
                            down: false,
                        });
                    }
                }
                Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    self.sync_modifiers(*modifiers, &mut out);
                    let typed =
                        types_text(*key) && !modifiers.ctrl && !modifiers.alt && !modifiers.mac_cmd;
                    if typed {
                        continue;
                    }
                    if let Some(keysym) = keysym_for_key(*key) {
                        self.track(keysym, *pressed);
                        out.push(KeyAction {
                            keysym,
                            down: *pressed,
                        });
                    }
                }
                _ => {}
            }
        }
        self.sync_modifiers(frame_mods, &mut out);
        out
    }

    /// Release everything still held (focus lost, disconnect).
    pub fn release_all(&mut self) -> Vec<KeyAction> {
        self.mods = Modifiers::NONE;
        self.held
            .drain(..)
            .rev()
            .map(|keysym| KeyAction {
                keysym,
                down: false,
            })
            .collect()
    }

    fn track(&mut self, keysym: u32, down: bool) {
        if down {
            if !self.held.contains(&keysym) {
                self.held.push(keysym);
            }
        } else {
            self.held.retain(|&k| k != keysym);
        }
    }

    fn sync_modifiers(&mut self, now: Modifiers, out: &mut Vec<KeyAction>) {
        let wanted = [
            (keysym::SHIFT_L, now.shift),
            (keysym::CONTROL_L, now.ctrl),
            (keysym::ALT_L, now.alt),
            (keysym::SUPER_L, now.mac_cmd),
        ];
        let had = [
            self.mods.shift,
            self.mods.ctrl,
            self.mods.alt,
            self.mods.mac_cmd,
        ];
        for ((keysym, down), had) in wanted.into_iter().zip(had) {
            if down != had {
                self.track(keysym, down);
                out.push(KeyAction { keysym, down });
            }
        }
        self.mods = now;
    }
}

/// A pointer state to send: button mask and remote position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerAction {
    pub buttons: u8,
    pub x: u16,
    pub y: u16,
}

/// RFB button mask bits.
pub mod button {
    pub const LEFT: u8 = 1;
    pub const MIDDLE: u8 = 1 << 1;
    pub const RIGHT: u8 = 1 << 2;
    pub const WHEEL_UP: u8 = 1 << 3;
    pub const WHEEL_DOWN: u8 = 1 << 4;
    pub const WHEEL_LEFT: u8 = 1 << 5;
    pub const WHEEL_RIGHT: u8 = 1 << 6;
    pub const BACK: u8 = 1 << 7;
}

fn button_bit(b: PointerButton) -> Option<u8> {
    match b {
        PointerButton::Primary => Some(button::LEFT),
        PointerButton::Middle => Some(button::MIDDLE),
        PointerButton::Secondary => Some(button::RIGHT),
        PointerButton::Extra1 => Some(button::BACK),
        PointerButton::Extra2 => None,
    }
}

/// Wheel steps for one scroll event as (vertical, horizontal). Positive
/// vertical means the wheel turned up (egui and winit report the content
/// moving down); positive horizontal means the content moves right, which is
/// a scroll to the left.
#[must_use]
pub fn wheel_steps(unit: MouseWheelUnit, delta: Vec2) -> (i32, i32) {
    let per_step = match unit {
        MouseWheelUnit::Line => 1.0,
        MouseWheelUnit::Point => 40.0,
        MouseWheelUnit::Page => 0.1,
    };
    // A scroll of any size is at least one step.
    let steps = |d: f32| {
        if d == 0.0 {
            0
        } else {
            let n = (d.abs() / per_step).round().clamp(1.0, 20.0);
            // Bounded to 1..=20, so the conversion is exact.
            #[allow(clippy::cast_possible_truncation)]
            let n = n as i32;
            if d > 0.0 { n } else { -n }
        }
    };
    (steps(delta.y), steps(delta.x))
}

/// Maps screen positions on the displayed picture to remote coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Canvas {
    /// Where the picture is drawn, in points.
    pub image: Rect,
    /// Remote framebuffer size in pixels.
    pub remote: (u16, u16),
}

impl Canvas {
    /// The largest rectangle with the remote's aspect ratio that fits in
    /// `available`, centred (letterboxed); with `one_to_one` it is the remote
    /// size in points, centred (and may exceed `available`).
    #[must_use]
    pub fn fit(available: Rect, remote: (u16, u16), one_to_one: bool) -> Self {
        let size = Vec2::new(f32::from(remote.0.max(1)), f32::from(remote.1.max(1)));
        let scale = if one_to_one {
            1.0
        } else {
            (available.width() / size.x).min(available.height() / size.y)
        };
        Self {
            image: Rect::from_center_size(available.center(), size * scale),
            remote,
        }
    }

    /// Remote coordinates for a screen position; positions outside the
    /// picture clamp to its edge, so dragging past the border keeps working.
    #[must_use]
    pub fn to_remote(&self, pos: Pos2) -> (u16, u16) {
        let axis = |p: f32, lo: f32, len: f32, max: u16| {
            let max_px = f32::from(max.saturating_sub(1));
            let t = if len > 0.0 { (p - lo) / len } else { 0.0 };
            // Clamped to 0..=max_px (< 65536) first, so the cast is exact.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let v = (t * f32::from(max)).floor().clamp(0.0, max_px) as u16;
            v
        };
        (
            axis(pos.x, self.image.min.x, self.image.width(), self.remote.0),
            axis(pos.y, self.image.min.y, self.image.height(), self.remote.1),
        )
    }
}

/// Builds pointer actions from egui events, remembering the button state.
#[derive(Debug, Default)]
pub struct PointerTranslator {
    buttons: u8,
}

impl PointerTranslator {
    /// Translate one frame's events over `canvas`. `pos` is where the pointer
    /// is now (if it is in the window). Consecutive move-only updates collapse
    /// to the last one.
    pub fn translate(
        &mut self,
        events: &[Event],
        canvas: &Canvas,
        pos: Option<Pos2>,
    ) -> Vec<PointerAction> {
        let mut out: Vec<PointerAction> = Vec::new();
        let mut here = pos.map_or((0, 0), |p| canvas.to_remote(p));
        // Whether the last action in `out` is a plain move, which a following
        // move replaces. Presses, releases and wheel steps always stay: a
        // press must happen where the pointer was when it was pressed.
        let mut last_is_move = false;
        for event in events {
            let was_move = std::mem::replace(&mut last_is_move, false);
            last_is_move = was_move && matches!(event, Event::PointerMoved(_));
            match event {
                Event::PointerMoved(p) => {
                    here = canvas.to_remote(*p);
                    let action = PointerAction {
                        buttons: self.buttons,
                        x: here.0,
                        y: here.1,
                    };
                    match out.last_mut() {
                        Some(last) if last_is_move => *last = action,
                        _ => out.push(action),
                    }
                    last_is_move = true;
                }
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } => {
                    here = canvas.to_remote(*pos);
                    if let Some(bit) = button_bit(*button) {
                        if *pressed {
                            self.buttons |= bit;
                        } else {
                            self.buttons &= !bit;
                        }
                        out.push(PointerAction {
                            buttons: self.buttons,
                            x: here.0,
                            y: here.1,
                        });
                    }
                }
                Event::MouseWheel { unit, delta, .. } => {
                    let (vertical, horizontal) = wheel_steps(*unit, *delta);
                    for (steps, up, down) in [
                        (vertical, button::WHEEL_UP, button::WHEEL_DOWN),
                        (horizontal, button::WHEEL_LEFT, button::WHEEL_RIGHT),
                    ] {
                        let bit = if steps > 0 { up } else { down };
                        for _ in 0..steps.unsigned_abs() {
                            out.push(PointerAction {
                                buttons: self.buttons | bit,
                                x: here.0,
                                y: here.1,
                            });
                            out.push(PointerAction {
                                buttons: self.buttons,
                                x: here.0,
                                y: here.1,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// True while any button is held, so a drag that leaves the window still
    /// delivers its release.
    #[must_use]
    pub fn has_buttons(&self) -> bool {
        self.buttons != 0
    }

    /// Release held buttons (focus lost, disconnect) at `(x, y)`.
    pub fn release_all(&mut self, (x, y): (u16, u16)) -> Option<PointerAction> {
        let had = std::mem::take(&mut self.buttons);
        (had != 0).then_some(PointerAction { buttons: 0, x, y })
    }
}

#[cfg(test)]
mod tests {
    use egui::{Modifiers, Pos2, Vec2};

    use super::*;

    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };

    fn act(keysym: u32, down: bool) -> KeyAction {
        KeyAction { keysym, down }
    }

    #[test]
    fn characters_map_to_keysyms() {
        assert_eq!(keysym_for_char('a'), Some(0x61));
        assert_eq!(keysym_for_char('A'), Some(0x41));
        assert_eq!(keysym_for_char(' '), Some(0x20));
        assert_eq!(keysym_for_char('\u{e9}'), Some(0xE9)); // e acute, Latin-1
        assert_eq!(keysym_for_char('\u{20ac}'), Some(0x0100_20AC)); // euro sign
        assert_eq!(keysym_for_char('\n'), None);
        assert_eq!(keysym_for_char('\u{7f}'), None);
    }

    #[test]
    fn keys_map_to_keysyms() {
        assert_eq!(keysym_for_key(Key::Enter), Some(0xFF0D));
        assert_eq!(keysym_for_key(Key::F5), Some(0xFFC2));
        assert_eq!(keysym_for_key(Key::F12), Some(0xFFC9));
        assert_eq!(keysym_for_key(Key::ArrowLeft), Some(0xFF51));
        assert_eq!(keysym_for_key(Key::A), Some(0x61), "unshifted");
        assert_eq!(keysym_for_key(Key::Num5), Some(0x35));
        assert_eq!(keysym_for_key(Key::Minus), Some(0x2D));
    }

    #[test]
    fn typed_text_is_press_and_release() {
        let mut t = KeyTranslator::default();
        let out = t.translate(&[Event::Text("h\u{e9}".into())], Modifiers::NONE);
        assert_eq!(
            out,
            [
                act(0x68, true),
                act(0x68, false),
                act(0xE9, true),
                act(0xE9, false)
            ]
        );
    }

    #[test]
    fn plain_letter_keys_are_left_to_the_text_event() {
        let mut t = KeyTranslator::default();
        let events = [
            key(Key::A, true, Modifiers::NONE),
            Event::Text("a".into()),
            key(Key::A, false, Modifiers::NONE),
        ];
        assert_eq!(
            t.translate(&events, Modifiers::NONE),
            [act(0x61, true), act(0x61, false)]
        );
    }

    #[test]
    fn ctrl_c_sends_modifier_then_key_and_releases_in_order() {
        let mut t = KeyTranslator::default();
        assert_eq!(
            t.translate(&[key(Key::C, true, CTRL)], CTRL),
            [act(keysym::CONTROL_L, true), act(0x63, true)]
        );
        assert_eq!(
            t.translate(&[key(Key::C, false, CTRL)], Modifiers::NONE),
            [act(0x63, false), act(keysym::CONTROL_L, false)]
        );
    }

    #[test]
    fn special_keys_and_shift_are_forwarded_with_repeat() {
        let mut t = KeyTranslator::default();
        let out = t.translate(&[key(Key::Enter, true, SHIFT)], SHIFT);
        assert_eq!(out, [act(keysym::SHIFT_L, true), act(keysym::RETURN, true)]);
        let repeat = Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: SHIFT,
        };
        assert_eq!(t.translate(&[repeat], SHIFT), [act(keysym::RETURN, true)]);
    }

    #[test]
    fn modifier_only_changes_are_sent_at_the_end_of_the_frame() {
        let mut t = KeyTranslator::default();
        assert_eq!(t.translate(&[], SHIFT), [act(keysym::SHIFT_L, true)]);
        assert_eq!(t.translate(&[], SHIFT), [], "no change, no event");
        assert_eq!(
            t.translate(&[], Modifiers::NONE),
            [act(keysym::SHIFT_L, false)]
        );
    }

    #[test]
    fn release_all_lets_go_of_everything_held() {
        let mut t = KeyTranslator::default();
        t.translate(&[key(Key::ArrowDown, true, CTRL)], CTRL);
        assert_eq!(
            t.release_all(),
            [act(keysym::DOWN, false), act(keysym::CONTROL_L, false)]
        );
        assert_eq!(t.release_all(), []);
        // The translator starts clean: pressing Ctrl again is a fresh press.
        assert_eq!(t.translate(&[], CTRL), [act(keysym::CONTROL_L, true)]);
    }

    fn canvas() -> Canvas {
        // 1000 x 500 area, 400 x 400 remote: a 500 x 500 image centred, x 250..750.
        Canvas::fit(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0)),
            (400, 400),
            false,
        )
    }

    #[test]
    fn fit_letterboxes_and_maps_positions() {
        let c = canvas();
        assert_eq!(
            c.image,
            Rect::from_min_size(Pos2::new(250.0, 0.0), Vec2::new(500.0, 500.0))
        );
        assert_eq!(c.to_remote(Pos2::new(500.0, 250.0)), (200, 200));
        assert_eq!(c.to_remote(Pos2::new(250.0, 0.0)), (0, 0));
        assert_eq!(c.to_remote(Pos2::new(749.9, 499.9)), (399, 399));
        // Outside the picture clamps to the edge.
        assert_eq!(c.to_remote(Pos2::new(-50.0, 900.0)), (0, 399));
        assert_eq!(c.to_remote(Pos2::new(5000.0, -3.0)), (399, 0));
    }

    #[test]
    fn one_to_one_uses_the_remote_size() {
        let c = Canvas::fit(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 100.0)),
            (400, 200),
            true,
        );
        assert_eq!(c.image.size(), Vec2::new(400.0, 200.0));
        assert_eq!(c.image.center(), Pos2::new(50.0, 50.0));
        assert_eq!(c.to_remote(Pos2::new(50.0, 50.0)), (200, 100));
    }

    #[test]
    fn wheel_steps_have_at_least_one_step_and_a_ceiling() {
        use MouseWheelUnit::{Line, Page, Point};
        assert_eq!(wheel_steps(Line, Vec2::new(0.0, 1.0)), (1, 0));
        assert_eq!(wheel_steps(Line, Vec2::new(0.0, -2.0)), (-2, 0));
        assert_eq!(wheel_steps(Point, Vec2::new(0.0, 3.0)), (1, 0));
        assert_eq!(wheel_steps(Point, Vec2::new(0.0, 120.0)), (3, 0));
        assert_eq!(wheel_steps(Point, Vec2::new(-40.0, 0.0)), (0, -1));
        assert_eq!(wheel_steps(Page, Vec2::new(0.0, 1.0)), (10, 0));
        assert_eq!(wheel_steps(Line, Vec2::new(0.0, 1e9)), (20, 0));
        assert_eq!(wheel_steps(Line, Vec2::ZERO), (0, 0));
    }

    fn click(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn pointer_moves_collapse_and_buttons_are_remembered() {
        let c = canvas();
        let mut t = PointerTranslator::default();
        let p = |x, y| Event::PointerMoved(Pos2::new(x, y));
        // Three moves in one frame become the last one.
        let out = t.translate(
            &[p(300.0, 100.0), p(400.0, 100.0), p(500.0, 250.0)],
            &c,
            None,
        );
        assert_eq!(
            out,
            [PointerAction {
                buttons: 0,
                x: 200,
                y: 200
            }]
        );
        // Press, drag, release.
        let out = t.translate(
            &[
                click(Pos2::new(500.0, 250.0), PointerButton::Primary, true),
                p(600.0, 250.0),
                p(700.0, 250.0),
                click(Pos2::new(700.0, 250.0), PointerButton::Primary, false),
            ],
            &c,
            None,
        );
        assert_eq!(
            out,
            [
                PointerAction {
                    buttons: 1,
                    x: 200,
                    y: 200
                },
                PointerAction {
                    buttons: 1,
                    x: 360,
                    y: 200
                },
                PointerAction {
                    buttons: 0,
                    x: 360,
                    y: 200
                },
            ]
        );
    }

    #[test]
    fn buttons_and_wheel_use_the_rfb_bits() {
        let c = canvas();
        let mut t = PointerTranslator::default();
        let at = Pos2::new(500.0, 250.0);
        let out = t.translate(
            &[
                click(at, PointerButton::Secondary, true),
                click(at, PointerButton::Middle, true),
                click(at, PointerButton::Extra1, true),
            ],
            &c,
            None,
        );
        assert_eq!(
            out.iter().map(|a| a.buttons).collect::<Vec<_>>(),
            [
                button::RIGHT,
                button::RIGHT | button::MIDDLE,
                button::RIGHT | button::MIDDLE | button::BACK
            ]
        );
        let mut t = PointerTranslator::default();
        let wheel = Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: Vec2::new(0.0, -2.0),
            modifiers: Modifiers::NONE,
        };
        let out = t.translate(&[wheel], &c, Some(at));
        assert_eq!(
            out.iter().map(|a| a.buttons).collect::<Vec<_>>(),
            [button::WHEEL_DOWN, 0, button::WHEEL_DOWN, 0]
        );
        assert!(out.iter().all(|a| (a.x, a.y) == (200, 200)));
    }

    #[test]
    fn release_all_lifts_held_buttons_once() {
        let c = canvas();
        let mut t = PointerTranslator::default();
        t.translate(
            &[click(Pos2::new(500.0, 250.0), PointerButton::Primary, true)],
            &c,
            None,
        );
        assert_eq!(
            t.release_all((5, 6)),
            Some(PointerAction {
                buttons: 0,
                x: 5,
                y: 6
            })
        );
        assert_eq!(t.release_all((5, 6)), None);
    }
}
