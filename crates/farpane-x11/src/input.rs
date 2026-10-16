use std::{
    collections::HashMap,
    io,
    sync::{Mutex, mpsc::Sender},
};

use rfb_server::Input;
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{ConnectionExt as _, Keycode, Window},
        xtest::ConnectionExt as _,
    },
    wrapper::ConnectionExt as _,
};

use crate::{Conn, io_err};

/// X event codes for `XTestFakeInput` (X11 protocol encoding).
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;

const SHIFT_L: u32 = 0xFFE1;
const SHIFT_R: u32 = 0xFFE2;

/// Injects keyboard and pointer input through `XTest`.
pub struct X11Input {
    conn: Conn,
    root: Window,
    state: Mutex<State>,
    /// Text clients copied, for the capture side to put on the clipboard.
    clipboard: Mutex<Sender<String>>,
}

impl std::fmt::Debug for X11Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Input").finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct State {
    /// keysym -> (keycode, needs Shift) from the current keyboard mapping.
    keys: HashMap<u32, (Keycode, bool)>,
    /// A keycode with no symbols, free to borrow for a keysym nothing maps.
    spare: Option<Keycode>,
    /// The borrowed keycode while its key is down: (keycode, keysym).
    borrowed: Option<(Keycode, u32)>,
    /// Keysyms currently held, to undo them if the client goes away.
    shift_down: bool,
    buttons: u8,
    per_keycode: u8,
}

impl X11Input {
    pub(crate) fn new(
        conn: Conn,
        screen_num: usize,
        clipboard: Sender<String>,
    ) -> io::Result<Self> {
        let root = conn.setup().roots[screen_num].root;
        conn.xtest_get_version(2, 2)
            .map_err(io_err)?
            .reply()
            .map_err(|e| io::Error::other(format!("the XTest extension is missing: {e}")))?;
        let input = Self {
            conn,
            root,
            state: Mutex::new(State::default()),
            clipboard: Mutex::new(clipboard),
        };
        {
            let mut st = input
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            input.load_keymap(&mut st)?;
        }
        Ok(input)
    }

    fn load_keymap(&self, st: &mut State) -> io::Result<()> {
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(io_err)?
            .reply()
            .map_err(io_err)?;
        let per = usize::from(map.keysyms_per_keycode).max(1);
        st.per_keycode = map.keysyms_per_keycode;
        st.keys.clear();
        st.spare = None;
        for (i, syms) in map.keysyms.chunks(per).enumerate() {
            let Ok(code) = Keycode::try_from(usize::from(min) + i) else {
                break;
            };
            if syms.iter().all(|&s| s == 0) {
                // Prefer a spare high in the range: low codes are the real keys.
                st.spare = Some(code);
                continue;
            }
            for (column, &sym) in syms.iter().take(2).enumerate() {
                if sym != 0 {
                    // The first mapping found for a symbol wins, and an
                    // unshifted one beats a shifted one.
                    let entry = st.keys.entry(sym).or_insert((code, column == 1));
                    if column == 0 && entry.1 {
                        *entry = (code, false);
                    }
                }
            }
        }
        Ok(())
    }

    /// The keycode this keysym would be pressed with, if the keyboard layout
    /// has it. Symbols without one are borrowed onto a spare keycode.
    #[must_use]
    pub fn keycode_for(&self, keysym: u32) -> Option<u8> {
        let st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.keys.get(&keysym).map(|k| k.0)
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> io::Result<()> {
        self.conn
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .map_err(io_err)?;
        Ok(())
    }

    fn key_event(&self, st: &mut State, keysym: u32, down: bool) -> io::Result<()> {
        match keysym {
            SHIFT_L | SHIFT_R => st.shift_down = down,
            _ => {}
        }
        let (keycode, needs_shift) = if let Some(&k) = st.keys.get(&keysym) {
            k
        } else if let Some((code, sym)) = st.borrowed.filter(|&(_, s)| s == keysym) {
            // The key whose release this is.
            let _ = sym;
            (code, false)
        } else if let (true, Some(spare)) = (down, st.spare) {
            // Borrow the spare keycode for a symbol nothing maps (a euro sign
            // on a US keyboard, say). One still held down is let go first, so
            // it is not left pressed under a different symbol.
            if let Some((old, _)) = st.borrowed {
                self.fake(KEY_RELEASE, old, 0, 0)?;
            }
            self.conn
                .change_keyboard_mapping(
                    1,
                    spare,
                    st.per_keycode.max(1),
                    &vec![keysym; usize::from(st.per_keycode.max(1))],
                )
                .map_err(io_err)?;
            self.conn.sync().map_err(io_err)?;
            st.borrowed = Some((spare, keysym));
            (spare, false)
        } else {
            return Ok(()); // nothing to press, and nothing to release
        };
        // A symbol that lives on the shifted level needs Shift held, unless
        // the client already holds it.
        let wrap_shift = needs_shift && !st.shift_down;
        let shift_code = st.keys.get(&SHIFT_L).map(|k| k.0);
        if down {
            if let (true, Some(s)) = (wrap_shift, shift_code) {
                self.fake(KEY_PRESS, s, 0, 0)?;
            }
            self.fake(KEY_PRESS, keycode, 0, 0)?;
            if let (true, Some(s)) = (wrap_shift, shift_code) {
                self.fake(KEY_RELEASE, s, 0, 0)?;
            }
        } else {
            self.fake(KEY_RELEASE, keycode, 0, 0)?;
            if st.borrowed.is_some_and(|(c, _)| c == keycode) {
                // Give the borrowed keycode back.
                self.conn
                    .change_keyboard_mapping(
                        1,
                        keycode,
                        st.per_keycode.max(1),
                        &vec![0; usize::from(st.per_keycode.max(1))],
                    )
                    .map_err(io_err)?;
                st.borrowed = None;
            }
        }
        self.conn.flush().map_err(io_err)
    }

    fn pointer_event(&self, st: &mut State, x: u16, y: u16, buttons: u8) -> io::Result<()> {
        self.fake(
            MOTION_NOTIFY,
            0,
            i16::try_from(x).unwrap_or(i16::MAX),
            i16::try_from(y).unwrap_or(i16::MAX),
        )?;
        // RFB bits 0..=2 are left, middle, right = X buttons 1..=3; bits 3
        // and 4 are the wheel (4 and 5), which are clicks rather than states.
        for bit in 0..8u8 {
            let mask = 1 << bit;
            let (was, is) = (st.buttons & mask != 0, buttons & mask != 0);
            let button = match bit {
                0..=6 => bit + 1,
                _ => continue,
            };
            if is && !was {
                self.fake(BUTTON_PRESS, button, 0, 0)?;
                if (3..=6).contains(&bit) {
                    self.fake(BUTTON_RELEASE, button, 0, 0)?;
                }
            } else if !is && was && bit < 3 {
                self.fake(BUTTON_RELEASE, button, 0, 0)?;
            }
        }
        st.buttons = buttons;
        self.conn.flush().map_err(io_err)
    }
}

impl Input for X11Input {
    fn key(&self, keysym: u32, down: bool) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A failed injection means the display is gone; the capture side
        // notices that and ends the server.
        let _ = self.key_event(&mut st, keysym, down);
    }

    fn pointer(&self, x: u16, y: u16, buttons: u8) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = self.pointer_event(&mut st, x, y, buttons);
    }

    fn clipboard(&self, text: &str) {
        // The capture side owns the selection and answers pastes; if it is
        // gone the server is ending anyway.
        let _ = self
            .clipboard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .send(text.to_owned());
    }
}
