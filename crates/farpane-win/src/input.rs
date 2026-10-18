use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

use rfb_server::Input;
use windows::Win32::{
    Foundation::{GlobalFree, HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard,
            SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Ole::CF_UNICODETEXT,
    },
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAP_VIRTUAL_KEY_TYPE,
            MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
            MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
            MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY, VkKeyScanW,
        },
        WindowsAndMessaging::{
            GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN,
        },
    },
};

use crate::keys::{self, Modifier};

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
/// One wheel notch.
const WHEEL_DELTA: i32 = 120;

/// Injects keyboard, pointer and clipboard input into the interactive
/// desktop.
pub struct WindowsInput {
    state: Mutex<State>,
    clip_own: Arc<AtomicU32>,
}

impl std::fmt::Debug for WindowsInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsInput").finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct State {
    shift: bool,
    ctrl: bool,
    alt: bool,
    buttons: u8,
}

impl WindowsInput {
    pub(crate) fn new(clip_own: Arc<AtomicU32>) -> Self {
        Self {
            state: Mutex::new(State::default()),
            clip_own,
        }
    }
}

fn send(inputs: &[INPUT]) {
    crate::desktop::follow();
    if inputs.is_empty() {
        return;
    }
    // SAFETY: `inputs` is a valid slice of fully initialised INPUT structs
    // and the size passed is that of one element.
    unsafe {
        // A refusal (the secure desktop is up, or a higher-integrity window
        // has focus) cannot be acted on here.
        SendInput(inputs, i32::try_from(size_of::<INPUT>()).unwrap_or(40));
    }
}

fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn vk_event(vk: u16, extended: bool, down: bool) -> INPUT {
    // SAFETY: no preconditions.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAP_VIRTUAL_KEY_TYPE(0)) };
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    keyboard(vk, u16::try_from(scan).unwrap_or(0), flags)
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data.cast_unsigned(),
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

impl WindowsInput {
    fn type_character(st: &State, ch: char, down: bool) {
        let mut units = [0u16; 2];
        let units = ch.encode_utf16(&mut units);
        let scan = if ch.len_utf16() == 1 {
            // SAFETY: no preconditions.
            i32::from(unsafe { VkKeyScanW(units[0]) })
        } else {
            -1
        };
        // A dead key (the accent keys of many European layouts) would wait
        // for the next letter and combine with it; send those as text too.
        // SAFETY: no preconditions.
        let dead = scan != -1
            && unsafe { MapVirtualKeyW((scan & 0xFF).cast_unsigned(), MAP_VIRTUAL_KEY_TYPE(2)) }
                & 0x8000_0000
                != 0;
        if scan == -1 || dead {
            // No key makes this character on the current layout, or only a
            // dead key does: send it as Unicode text (the release mirrors it).
            let inputs: Vec<INPUT> = units
                .iter()
                .map(|&u| {
                    let mut flags = KEYEVENTF_UNICODE;
                    if !down {
                        flags |= KEYEVENTF_KEYUP;
                    }
                    keyboard(0, u, flags)
                })
                .collect();
            send(&inputs);
            return;
        }
        let vk = u16::try_from(scan & 0xFF).unwrap_or(0);
        let needs = (scan >> 8) & 0xFF;
        if !down {
            send(&[vk_event(vk, false, false)]);
            return;
        }
        // Hold the modifiers this character needs on the layout (AltGr is
        // Ctrl + Alt), unless the client already holds them.
        let wrap: Vec<u16> = [
            (needs & 1 != 0 && !st.shift, VK_SHIFT),
            (needs & 2 != 0 && !st.ctrl, VK_CONTROL),
            (needs & 4 != 0 && !st.alt, VK_MENU),
        ]
        .into_iter()
        .filter_map(|(wanted, code)| wanted.then_some(code))
        .collect();
        let mut inputs: Vec<INPUT> = wrap.iter().map(|&m| vk_event(m, false, true)).collect();
        inputs.push(vk_event(vk, false, true));
        inputs.extend(wrap.iter().rev().map(|&m| vk_event(m, false, false)));
        send(&inputs);
    }
}

impl Input for WindowsInput {
    fn key(&self, keysym: u32, down: bool) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match keys::modifier(keysym) {
            Some(Modifier::Shift) => st.shift = down,
            Some(Modifier::Control) => st.ctrl = down,
            Some(Modifier::Alt) => st.alt = down,
            None => {}
        }
        if let Some(special) = keys::special(keysym) {
            send(&[vk_event(special.vk, special.extended, down)]);
        } else if let Some(ch) = keys::character(keysym) {
            Self::type_character(&st, ch, down);
        }
    }

    fn pointer(&self, x: u16, y: u16, buttons: u8) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: GetSystemMetrics has no preconditions.
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        // Absolute coordinates run 0..=65535 over the whole virtual desktop.
        let scale = |pos: i32, origin: i32, size: i32| {
            let span = i64::from((size - 1).max(1));
            let rel = i64::from(pos - origin).clamp(0, span);
            i32::try_from((rel * 65535 + span / 2) / span).unwrap_or(65535)
        };
        let (dx, dy) = (
            scale(i32::from(x) + vx, vx, vw),
            scale(i32::from(y) + vy, vy, vh),
        );
        let mut inputs = vec![mouse(
            dx,
            dy,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )];
        let pair = [
            (0, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            (1, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
            (2, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        ];
        for (bit, down, up) in pair {
            let (was, is) = (st.buttons & (1 << bit) != 0, buttons & (1 << bit) != 0);
            if is && !was {
                inputs.push(mouse(0, 0, 0, down));
            } else if !is && was {
                inputs.push(mouse(0, 0, 0, up));
            }
        }
        // Bits 3 / 4 are wheel up / down, 5 / 6 left / right: clicks, not states.
        for (bit, flags, delta) in [
            (3, MOUSEEVENTF_WHEEL, WHEEL_DELTA),
            (4, MOUSEEVENTF_WHEEL, -WHEEL_DELTA),
            (5, MOUSEEVENTF_HWHEEL, -WHEEL_DELTA),
            (6, MOUSEEVENTF_HWHEEL, WHEEL_DELTA),
        ] {
            if buttons & (1 << bit) != 0 && st.buttons & (1 << bit) == 0 {
                inputs.push(mouse(0, 0, delta, flags));
            }
        }
        st.buttons = buttons;
        send(&inputs);
    }

    fn clipboard(&self, text: &str) {
        if set_clipboard_text(text) {
            // SAFETY: no preconditions.
            self.clip_own
                .store(unsafe { GetClipboardSequenceNumber() }, Ordering::Relaxed);
        }
    }
}

/// Put `text` on the clipboard as Unicode text with CRLF line ends.
fn set_clipboard_text(text: &str) -> bool {
    let mut wide: Vec<u16> = text
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .encode_utf16()
        .collect();
    wide.push(0);
    let bytes = wide.len() * size_of::<u16>();
    // SAFETY: the global block is sized for `wide`, locked only to copy into
    // it, and freed here unless the clipboard took it over; the clipboard is
    // closed on every path.
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return false;
        }
        let ok = (|| {
            EmptyClipboard().ok()?;
            let global: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes).ok()?;
            let ptr = GlobalLock(global).cast::<u16>();
            if ptr.is_null() {
                let _ = GlobalFree(Some(global));
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(global);
            if SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(global.0))).is_err() {
                let _ = GlobalFree(Some(global));
                return None;
            }
            Some(())
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}
