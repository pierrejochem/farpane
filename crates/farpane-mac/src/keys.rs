//! X11 keysyms to macOS keyboard events.
//!
//! RFB sends keysyms: the symbol a key produced, not the key. macOS wants
//! virtual key codes, which are positions on an ANSI keyboard (`kVK_*` in
//! `HIToolbox/Events.h`). Letters and symbols are layout dependent there, so
//! the mapper uses key codes only where an application reads the code, which
//! is shortcuts (Command or Control held), and sends everything else as the
//! character itself (`CGEventKeyboardSetUnicodeString`), which works in any
//! layout and for any Unicode character.
//!
//! The mapper keeps the state a keysym stream does not carry: which modifiers
//! are down (every event posted to macOS carries the modifier flags) and which
//! key code went down for a keysym (so its release matches).

use std::collections::HashMap;

/// `CGEventFlags` bits.
pub mod flags {
    pub const CAPS_LOCK: u64 = 0x0001_0000;
    pub const SHIFT: u64 = 0x0002_0000;
    pub const CONTROL: u64 = 0x0004_0000;
    pub const OPTION: u64 = 0x0008_0000;
    pub const COMMAND: u64 = 0x0010_0000;
    pub const NUMERIC_PAD: u64 = 0x0020_0000;
    pub const SECONDARY_FN: u64 = 0x0080_0000;
}

/// Virtual key codes used outside the plain letter table.
pub mod code {
    pub const RETURN: u16 = 0x24;
    pub const TAB: u16 = 0x30;
    pub const SPACE: u16 = 0x31;
    pub const BACKSPACE: u16 = 0x33;
    pub const ESCAPE: u16 = 0x35;
    pub const COMMAND_RIGHT: u16 = 0x36;
    pub const COMMAND: u16 = 0x37;
    pub const SHIFT: u16 = 0x38;
    pub const CAPS_LOCK: u16 = 0x39;
    pub const OPTION: u16 = 0x3A;
    pub const CONTROL: u16 = 0x3B;
    pub const SHIFT_RIGHT: u16 = 0x3C;
    pub const OPTION_RIGHT: u16 = 0x3D;
    pub const CONTROL_RIGHT: u16 = 0x3E;
    pub const FORWARD_DELETE: u16 = 0x75;
    pub const HOME: u16 = 0x73;
    pub const END: u16 = 0x77;
    pub const PAGE_UP: u16 = 0x74;
    pub const PAGE_DOWN: u16 = 0x79;
    pub const LEFT: u16 = 0x7B;
    pub const RIGHT: u16 = 0x7C;
    pub const DOWN: u16 = 0x7D;
    pub const UP: u16 = 0x7E;
    pub const HELP: u16 = 0x72;
}

/// What to post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// A key with this virtual key code, pressed or released as `down` says.
    Code(u16),
    /// Type this character (press and release in one event pair; only sent
    /// when the key goes down).
    Text(char),
}

/// One keyboard event to post, with the modifier flags it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub kind: KeyKind,
    pub down: bool,
    pub flags: u64,
}

/// A modifier key's code and its bit in the mapper's state.
fn modifier(keysym: u32) -> Option<(u16, u16)> {
    // Bits: 0 shift L, 1 shift R, 2 control L, 3 control R, 4 option L,
    // 5 option R, 6 command L, 7 command R.
    Some(match keysym {
        0xFFE1 => (code::SHIFT, 1 << 0),
        0xFFE2 => (code::SHIFT_RIGHT, 1 << 1),
        0xFFE3 => (code::CONTROL, 1 << 2),
        0xFFE4 => (code::CONTROL_RIGHT, 1 << 3),
        0xFFE9 => (code::OPTION, 1 << 4),
        // Right Alt and AltGr (ISO_Level3_Shift).
        0xFFEA | 0xFE03 => (code::OPTION_RIGHT, 1 << 5),
        // Meta and Super are the Windows / Command keys of other systems.
        0xFFE7 | 0xFFEB => (code::COMMAND, 1 << 6),
        0xFFE8 | 0xFFEC => (code::COMMAND_RIGHT, 1 << 7),
        _ => return None,
    })
}

/// The key code of a key that has no character, with the flags macOS expects
/// on it (arrows and the keypad carry `SECONDARY_FN` / `NUMERIC_PAD`).
fn special(keysym: u32) -> Option<(u16, u64)> {
    let fnk = flags::SECONDARY_FN;
    let pad = flags::NUMERIC_PAD;
    Some(match keysym {
        0xFF08 => (code::BACKSPACE, 0),
        0xFF09 | 0xFE20 => (code::TAB, 0),
        0xFF0D => (code::RETURN, 0),
        0xFF1B => (code::ESCAPE, 0),
        0xFFFF => (code::FORWARD_DELETE, fnk),
        0xFF50 => (code::HOME, fnk),
        0xFF51 => (code::LEFT, fnk | pad),
        0xFF52 => (code::UP, fnk | pad),
        0xFF53 => (code::RIGHT, fnk | pad),
        0xFF54 => (code::DOWN, fnk | pad),
        0xFF55 => (code::PAGE_UP, fnk),
        0xFF56 => (code::PAGE_DOWN, fnk),
        0xFF57 => (code::END, fnk),
        0xFF63 => (code::HELP, fnk),
        // F1 to F12, in the order of their (scattered) key codes.
        0xFFBE => (0x7A, fnk),
        0xFFBF => (0x78, fnk),
        0xFFC0 => (0x63, fnk),
        0xFFC1 => (0x76, fnk),
        0xFFC2 => (0x60, fnk),
        0xFFC3 => (0x61, fnk),
        0xFFC4 => (0x62, fnk),
        0xFFC5 => (0x64, fnk),
        0xFFC6 => (0x65, fnk),
        0xFFC7 => (0x6D, fnk),
        0xFFC8 => (0x67, fnk),
        0xFFC9 => (0x6F, fnk),
        // F13 to F20.
        0xFFCA => (0x69, fnk),
        0xFFCB => (0x6B, fnk),
        0xFFCC => (0x71, fnk),
        0xFFCD => (0x6A, fnk),
        0xFFCE => (0x40, fnk),
        0xFFCF => (0x4F, fnk),
        0xFFD0 => (0x50, fnk),
        0xFFD1 => (0x5A, fnk),
        // Keypad: digits, operators, Enter.
        0xFFB0 => (0x52, pad),
        0xFFB1 => (0x53, pad),
        0xFFB2 => (0x54, pad),
        0xFFB3 => (0x55, pad),
        0xFFB4 => (0x56, pad),
        0xFFB5 => (0x57, pad),
        0xFFB6 => (0x58, pad),
        0xFFB7 => (0x59, pad),
        0xFFB8 => (0x5B, pad),
        0xFFB9 => (0x5C, pad),
        0xFFAA => (0x43, pad),
        0xFFAB => (0x45, pad),
        0xFFAD => (0x4E, pad),
        0xFFAE => (0x41, pad),
        0xFFAF => (0x4B, pad),
        0xFF8D => (0x4C, pad),
        0xFFBD => (0x51, pad),
        _ => return None,
    })
}

/// The character a keysym stands for: Latin-1 keysyms are the code point,
/// `0x0100_0000 + n` is Unicode `n`.
#[must_use]
pub fn character(keysym: u32) -> Option<char> {
    match keysym {
        0x20..=0x7E | 0xA0..=0xFF => char::from_u32(keysym),
        0x0100_0000..=0x0110_FFFF => char::from_u32(keysym - 0x0100_0000),
        _ => None,
    }
}

/// The key code of the key that types `c` on an ANSI keyboard (shifted
/// symbols give the key they share), for shortcuts. `None` for anything not
/// on a US keyboard.
#[must_use]
pub fn ansi_code(c: char) -> Option<u16> {
    let base = match c {
        '~' => '`',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        other => other.to_ascii_lowercase(),
    };
    Some(match base {
        'a' => 0x00,
        's' => 0x01,
        'd' => 0x02,
        'f' => 0x03,
        'h' => 0x04,
        'g' => 0x05,
        'z' => 0x06,
        'x' => 0x07,
        'c' => 0x08,
        'v' => 0x09,
        'b' => 0x0B,
        'q' => 0x0C,
        'w' => 0x0D,
        'e' => 0x0E,
        'r' => 0x0F,
        'y' => 0x10,
        't' => 0x11,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '6' => 0x16,
        '5' => 0x17,
        '=' => 0x18,
        '9' => 0x19,
        '7' => 0x1A,
        '-' => 0x1B,
        '8' => 0x1C,
        '0' => 0x1D,
        ']' => 0x1E,
        'o' => 0x1F,
        'u' => 0x20,
        '[' => 0x21,
        'i' => 0x22,
        'p' => 0x23,
        'l' => 0x25,
        'j' => 0x26,
        '\'' => 0x27,
        'k' => 0x28,
        ';' => 0x29,
        '\\' => 0x2A,
        ',' => 0x2B,
        '/' => 0x2C,
        'n' => 0x2D,
        'm' => 0x2E,
        '.' => 0x2F,
        ' ' => code::SPACE,
        '`' => 0x32,
        _ => return None,
    })
}

/// Turns a stream of keysym presses and releases into events to post.
#[derive(Debug, Default)]
pub struct KeyMapper {
    /// Modifier keys down (bits as in [`modifier`]).
    held: u16,
    caps: bool,
    /// The key code sent for each keysym that is down, so its release sends
    /// the same one.
    down: HashMap<u32, (u16, u64)>,
}

impl KeyMapper {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The flags of the modifiers now down.
    #[must_use]
    pub fn flags(&self) -> u64 {
        let any = |mask: u16| self.held & mask != 0;
        let mut f = 0;
        if any(0b11) {
            f |= flags::SHIFT;
        }
        if any(0b1100) {
            f |= flags::CONTROL;
        }
        if any(0b11_0000) {
            f |= flags::OPTION;
        }
        if any(0b1100_0000) {
            f |= flags::COMMAND;
        }
        if self.caps {
            f |= flags::CAPS_LOCK;
        }
        f
    }

    /// A key went down or up. Returns the events to post, in order (empty
    /// for a keysym that means nothing here, or a text key's release).
    pub fn key(&mut self, keysym: u32, down: bool) -> Vec<KeyEvent> {
        if let Some((key_code, bit)) = modifier(keysym) {
            if down {
                self.held |= bit;
            } else {
                self.held &= !bit;
            }
            return vec![KeyEvent {
                kind: KeyKind::Code(key_code),
                down,
                flags: self.flags(),
            }];
        }
        if keysym == 0xFFE5 {
            // Caps Lock: the flag follows presses.
            if down {
                self.caps = !self.caps;
            }
            return vec![KeyEvent {
                kind: KeyKind::Code(code::CAPS_LOCK),
                down,
                flags: self.flags(),
            }];
        }
        if !down {
            // A release matches whatever the press sent.
            return match self.down.remove(&keysym) {
                Some((key_code, extra)) => vec![KeyEvent {
                    kind: KeyKind::Code(key_code),
                    down: false,
                    flags: self.flags() | extra,
                }],
                None => Vec::new(),
            };
        }
        if let Some((key_code, extra)) = special(keysym) {
            self.down.insert(keysym, (key_code, extra));
            return vec![KeyEvent {
                kind: KeyKind::Code(key_code),
                down: true,
                flags: self.flags() | extra,
            }];
        }
        let Some(c) = character(keysym) else {
            return Vec::new();
        };
        let shortcut = self.flags() & (flags::COMMAND | flags::CONTROL) != 0;
        if let Some(key_code) = ansi_code(c).filter(|_| shortcut) {
            self.down.insert(keysym, (key_code, 0));
            return vec![KeyEvent {
                kind: KeyKind::Code(key_code),
                down: true,
                flags: self.flags(),
            }];
        }
        // The character itself; Shift and Option already shaped it.
        vec![KeyEvent {
            kind: KeyKind::Text(c),
            down: true,
            flags: self.flags() & !(flags::SHIFT | flags::OPTION),
        }]
    }

    /// Releases everything that is down (a client left, or sharing paused):
    /// no key may stay stuck.
    pub fn release_all(&mut self) -> Vec<KeyEvent> {
        let mut out = Vec::new();
        let mut keys: Vec<_> = self.down.drain().collect();
        keys.sort_by_key(|(sym, _)| *sym);
        for (_, (key_code, extra)) in keys {
            out.push(KeyEvent {
                kind: KeyKind::Code(key_code),
                down: false,
                flags: self.flags() | extra,
            });
        }
        // Modifiers last, each reported with the flags that remain.
        for (sym, bit) in [
            (0xFFE1, 1 << 0),
            (0xFFE2, 1 << 1),
            (0xFFE3, 1 << 2),
            (0xFFE4, 1 << 3),
            (0xFFE9, 1 << 4),
            (0xFFEA, 1 << 5),
            (0xFFEB, 1 << 6),
            (0xFFEC, 1 << 7),
        ] {
            if self.held & bit != 0 {
                out.extend(self.key(sym, false));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT_L: u32 = 0xFFE1;
    const CONTROL_L: u32 = 0xFFE3;
    const SUPER_L: u32 = 0xFFEB;

    fn kinds(events: &[KeyEvent]) -> Vec<(KeyKind, bool)> {
        events.iter().map(|e| (e.kind, e.down)).collect()
    }

    #[test]
    fn plain_characters_are_typed_as_text_and_release_is_silent() {
        let mut m = KeyMapper::new();
        assert_eq!(
            kinds(&m.key('a'.into(), true)),
            [(KeyKind::Text('a'), true)]
        );
        assert_eq!(m.key('a'.into(), false), []);
        // Unicode keysyms and Latin-1 beyond ASCII.
        assert_eq!(
            kinds(&m.key(0x0100_20AC, true)),
            [(KeyKind::Text('\u{20AC}'), true)]
        );
        assert_eq!(kinds(&m.key(0xE4, true)), [(KeyKind::Text('ä'), true)]);
    }

    #[test]
    fn shift_does_not_leak_into_text_but_is_tracked() {
        let mut m = KeyMapper::new();
        let shift = m.key(SHIFT_L, true);
        assert_eq!(kinds(&shift), [(KeyKind::Code(code::SHIFT), true)]);
        assert_eq!(shift[0].flags, flags::SHIFT);
        let text = m.key('A'.into(), true);
        assert_eq!(text[0].kind, KeyKind::Text('A'));
        assert_eq!(text[0].flags, 0);
        assert_eq!(m.key(SHIFT_L, false)[0].flags, 0);
    }

    #[test]
    fn shortcuts_use_key_codes_and_the_release_matches() {
        let mut m = KeyMapper::new();
        m.key(SUPER_L, true);
        let press = m.key('c'.into(), true);
        assert_eq!(kinds(&press), [(KeyKind::Code(0x08), true)]);
        assert_eq!(press[0].flags, flags::COMMAND);
        // The modifier is released first, then the key: the key still sends its own code.
        m.key(SUPER_L, false);
        let release = m.key('c'.into(), false);
        assert_eq!(kinds(&release), [(KeyKind::Code(0x08), false)]);
        assert_eq!(release[0].flags, 0);
    }

    #[test]
    fn control_shortcuts_and_shifted_symbols_share_the_base_key() {
        let mut m = KeyMapper::new();
        m.key(CONTROL_L, true);
        assert_eq!(
            kinds(&m.key('?'.into(), true)),
            [(KeyKind::Code(0x2C), true)]
        );
        // Something with no key on a US keyboard stays text even in a shortcut.
        assert_eq!(
            kinds(&m.key('ä' as u32, true)),
            [(KeyKind::Text('ä'), true)]
        );
    }

    #[test]
    fn special_keys_carry_their_flags() {
        let mut m = KeyMapper::new();
        let up = m.key(0xFF52, true);
        assert_eq!(up[0].kind, KeyKind::Code(code::UP));
        assert_eq!(up[0].flags, flags::SECONDARY_FN | flags::NUMERIC_PAD);
        let f1 = m.key(0xFFBE, true);
        assert_eq!(f1[0].kind, KeyKind::Code(0x7A));
        let enter = m.key(0xFF0D, true);
        assert_eq!(enter[0].kind, KeyKind::Code(code::RETURN));
        assert_eq!(enter[0].flags, 0);
        assert_eq!(
            kinds(&m.key(0xFF0D, false)),
            [(KeyKind::Code(code::RETURN), false)]
        );
    }

    #[test]
    fn caps_lock_toggles_its_flag_on_press() {
        let mut m = KeyMapper::new();
        assert_eq!(m.key(0xFFE5, true)[0].flags, flags::CAPS_LOCK);
        assert_eq!(m.key(0xFFE5, false)[0].flags, flags::CAPS_LOCK);
        assert_eq!(m.key(0xFFE5, true)[0].flags, 0);
    }

    #[test]
    fn alt_gr_and_both_sides_of_a_modifier() {
        let mut m = KeyMapper::new();
        m.key(0xFE03, true);
        assert_eq!(m.flags(), flags::OPTION);
        m.key(0xFFE9, true);
        m.key(0xFFEA, false);
        assert_eq!(m.flags(), flags::OPTION, "left Option still down");
        m.key(0xFFE9, false);
        assert_eq!(m.flags(), 0);
    }

    #[test]
    fn release_all_lets_go_of_keys_and_modifiers() {
        let mut m = KeyMapper::new();
        m.key(SUPER_L, true);
        m.key(0xFF51, true);
        m.key('c'.into(), true);
        let out = m.release_all();
        assert!(out.iter().all(|e| !e.down));
        assert_eq!(out.len(), 3);
        assert_eq!(out.last().unwrap().kind, KeyKind::Code(code::COMMAND));
        assert_eq!(m.flags(), 0);
        assert_eq!(m.release_all(), []);
    }

    #[test]
    fn unknown_keysyms_do_nothing() {
        let mut m = KeyMapper::new();
        assert_eq!(m.key(0xFD00, true), []);
        assert_eq!(m.key(0xFD00, false), []);
    }

    #[test]
    fn every_ascii_letter_digit_and_symbol_has_a_distinct_key() {
        let mut seen = std::collections::HashSet::new();
        for c in r"abcdefghijklmnopqrstuvwxyz1234567890-=[]\;',./` ".chars() {
            let key = ansi_code(c).unwrap_or_else(|| panic!("{c:?} has no code"));
            assert!(seen.insert(key), "{c:?} repeats code {key:#x}");
        }
    }
}
