//! Following the input desktop.
//!
//! Windows shows the user's windows on the "Default" desktop, but the lock
//! screen, the sign-in screen and UAC prompts on "Winlogon". A thread sees
//! (and can type into) only the desktop it is attached to, so the session
//! worker re-attaches whenever the input desktop changes. That works only for
//! a process with the rights of SYSTEM; an ordinary user's process is refused
//! the Winlogon desktop and simply stays where it is.

use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::HANDLE,
    System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetUserObjectInformationW,
        HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
    },
};

/// How often the input desktop is looked at.
const EVERY: Duration = Duration::from_millis(200);
/// `GENERIC_ALL`: what a window-system thread needs on a desktop to capture
/// from it and send input to it.
const ALL_ACCESS: u32 = 0x1000_0000;

struct Attached {
    checked: Instant,
    name: String,
    desktop: HDESK,
}

thread_local! {
    static ATTACHED: RefCell<Option<Attached>> = const { RefCell::new(None) };
}

fn name_of(desktop: HDESK) -> Option<String> {
    let mut buffer = [0u16; 256];
    let mut needed = 0u32;
    // SAFETY: the buffer is 512 bytes, which is the size passed; `desktop` is
    // a valid handle opened by the caller.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(buffer.as_mut_ptr().cast()),
            512,
            Some(&raw mut needed),
        )
    }
    .ok()?;
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// Attach the calling thread to the current input desktop. Returns true when
/// that moved the thread to a different desktop, so device contexts taken
/// before must be taken again. Cheap: it looks at most every 200 ms.
pub(crate) fn follow() -> bool {
    ATTACHED.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_some_and(|a| a.checked.elapsed() < EVERY) {
            return false;
        }
        // SAFETY: no preconditions; the handle is closed below or kept in the slot.
        let Ok(desktop) = (unsafe {
            OpenInputDesktop(
                DESKTOP_CONTROL_FLAGS(0),
                false,
                DESKTOP_ACCESS_FLAGS(ALL_ACCESS),
            )
        }) else {
            return false;
        };
        let name = name_of(desktop).unwrap_or_default();
        if let Some(a) = slot.as_mut()
            && a.name == name
        {
            a.checked = Instant::now();
            // SAFETY: `desktop` was opened above and is not used again.
            let _ = unsafe { CloseDesktop(desktop) };
            return false;
        }
        // SAFETY: `desktop` is valid. A thread that owns windows or hooks is
        // refused; then it simply stays on its desktop.
        if unsafe { SetThreadDesktop(desktop) }.is_err() {
            // SAFETY: as above.
            let _ = unsafe { CloseDesktop(desktop) };
            return false;
        }
        if let Some(old) = slot.replace(Attached {
            checked: Instant::now(),
            name,
            desktop,
        }) {
            // SAFETY: the thread left this desktop just now, so it can be closed.
            let _ = unsafe { CloseDesktop(old.desktop) };
        }
        true
    })
}
