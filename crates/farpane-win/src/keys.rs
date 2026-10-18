//! X11 keysyms (what RFB carries) to Windows virtual-key codes. Pure tables,
//! so they are tested on every platform.

/// A key with no character of its own: its virtual-key code and whether
/// Windows marks it as an extended key (the grey navigation block, right-hand
/// Ctrl and Alt, the Windows keys, keypad Enter and Divide).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Special {
    pub vk: u16,
    pub extended: bool,
}

#[allow(clippy::unnecessary_wraps)] // same type as `ext` and the match arms
const fn key(vk: u16) -> Option<Special> {
    Some(Special {
        vk,
        extended: false,
    })
}

#[allow(clippy::unnecessary_wraps)]
const fn ext(vk: u16) -> Option<Special> {
    Some(Special { vk, extended: true })
}

/// The virtual key for a keysym that is not a printable character.
#[must_use]
// The two ranges with arithmetic are at most 24 wide.
#[allow(clippy::cast_possible_truncation)]
pub fn special(keysym: u32) -> Option<Special> {
    match keysym {
        0xFF08 => key(0x08),                                     // BackSpace
        0xFF09 | 0xFE20 => key(0x09),                            // Tab, ISO_Left_Tab
        0xFF0D => key(0x0D),                                     // Return
        0xFF13 => key(0x13),                                     // Pause
        0xFF14 => key(0x91),                                     // Scroll_Lock
        0xFF1B => key(0x1B),                                     // Escape
        0xFF50 => ext(0x24),                                     // Home
        0xFF51 => ext(0x25),                                     // Left
        0xFF52 => ext(0x26),                                     // Up
        0xFF53 => ext(0x27),                                     // Right
        0xFF54 => ext(0x28),                                     // Down
        0xFF55 => ext(0x21),                                     // Page_Up
        0xFF56 => ext(0x22),                                     // Page_Down
        0xFF57 => ext(0x23),                                     // End
        0xFF61 => key(0x2C),                                     // Print
        0xFF63 => ext(0x2D),                                     // Insert
        0xFF67 => ext(0x5D),                                     // Menu
        0xFF7F => ext(0x90),                                     // Num_Lock
        0xFF8D => ext(0x0D),                                     // KP_Enter
        0xFFFF => ext(0x2E),                                     // Delete
        0xFFAA => key(0x6A),                                     // KP_Multiply
        0xFFAB => key(0x6B),                                     // KP_Add
        0xFFAD => key(0x6D),                                     // KP_Subtract
        0xFFAE => key(0x6E),                                     // KP_Decimal
        0xFFAF => ext(0x6F),                                     // KP_Divide
        0xFFB0..=0xFFB9 => key(0x60 + (keysym - 0xFFB0) as u16), // KP_0 .. KP_9
        0xFFBE..=0xFFD5 => key(0x70 + (keysym - 0xFFBE) as u16), // F1 .. F24
        0xFFE1 => key(0xA0),                                     // Shift_L
        0xFFE2 => key(0xA1),                                     // Shift_R
        0xFFE3 => key(0xA2),                                     // Control_L
        0xFFE4 => ext(0xA3),                                     // Control_R
        0xFFE5 => key(0x14),                                     // Caps_Lock
        0xFFE7 | 0xFFE9 => key(0xA4),                            // Meta_L, Alt_L
        0xFFE8 | 0xFFEA | 0xFE03 => ext(0xA5), // Meta_R, Alt_R, ISO_Level3_Shift (AltGr)
        0xFFEB => ext(0x5B),                   // Super_L
        0xFFEC => ext(0x5C),                   // Super_R
        _ => None,
    }
}

/// The character a keysym stands for: Latin-1 keysyms are their code point,
/// `0x0100_0000 + c` is any Unicode character.
#[must_use]
pub fn character(keysym: u32) -> Option<char> {
    match keysym {
        0x20..=0x7E | 0xA0..=0xFF => char::from_u32(keysym),
        0x0100_0100..=0x0110_FFFF => char::from_u32(keysym - 0x0100_0000),
        _ => None,
    }
}

/// Whether a keysym is one of the modifier keys the input code tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Control,
    Alt,
}

#[must_use]
pub fn modifier(keysym: u32) -> Option<Modifier> {
    match keysym {
        0xFFE1 | 0xFFE2 => Some(Modifier::Shift),
        0xFFE3 | 0xFFE4 => Some(Modifier::Control),
        0xFFE7..=0xFFEA | 0xFE03 => Some(Modifier::Alt),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_keys_and_keypad() {
        assert_eq!(special(0xFFBE), key(0x70)); // F1
        assert_eq!(special(0xFFC9), key(0x7B)); // F12
        assert_eq!(special(0xFFB0), key(0x60)); // KP_0
        assert_eq!(special(0xFFB9), key(0x69)); // KP_9
    }

    #[test]
    fn navigation_keys_are_extended() {
        for sym in [
            0xFF50, 0xFF51, 0xFF52, 0xFF53, 0xFF54, 0xFF55, 0xFF56, 0xFF57, 0xFF63, 0xFFFF,
        ] {
            assert!(special(sym).unwrap().extended, "{sym:#x}");
        }
        assert!(!special(0xFF0D).unwrap().extended); // Return
        assert!(special(0xFF8D).unwrap().extended); // KP_Enter
    }

    #[test]
    fn printable_characters_have_no_special_entry() {
        assert_eq!(special(u32::from(b'a')), None);
        assert_eq!(character(u32::from(b'a')), Some('a'));
        assert_eq!(character(0xE4), Some('ä'));
        assert_eq!(character(0x0100_20AC), Some('€'));
        assert_eq!(character(0xFF0D), None);
        assert_eq!(character(0x1F), None);
    }

    #[test]
    fn modifiers_are_recognised() {
        assert_eq!(modifier(0xFFE1), Some(Modifier::Shift));
        assert_eq!(modifier(0xFFE4), Some(Modifier::Control));
        assert_eq!(modifier(0xFFEA), Some(Modifier::Alt));
        assert_eq!(modifier(u32::from(b'a')), None);
    }
}
