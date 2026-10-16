//! The X11 backend against the real X server (`$DISPLAY`); skipped without one.

use std::sync::Mutex;

use farpane_grab::{GrabState, KeyboardGrab};
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::{
        Event,
        xproto::{ConnectionExt, CreateWindowAux, EventMask, GrabMode, GrabStatus, WindowClass},
    },
    rust_connection::RustConnection,
};

/// An X keyboard grab is global to the server, so these tests must not overlap.
static SERVER: Mutex<()> = Mutex::new(());

/// A mapped window on its own connection, or `None` without an X server.
fn window() -> Option<(RustConnection, u32)> {
    let (conn, screen_num) = RustConnection::connect(None).ok()?;
    let screen = &conn.setup().roots[screen_num];
    let win = conn.generate_id().ok()?;
    conn.create_window(
        screen.root_depth,
        win,
        screen.root,
        0,
        0,
        200,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
    )
    .ok()?;
    conn.map_window(win).ok()?;
    conn.flush().ok()?;
    // A grab needs a viewable window: wait until the server says it is mapped.
    loop {
        if let Event::MapNotify(e) = conn.wait_for_event().ok()? {
            if e.window == win {
                return Some((conn, win));
            }
        }
    }
}

/// Whether another client can grab the keyboard right now.
fn another_client_can_grab(other: &RustConnection, win: u32) -> bool {
    let reply = other
        .grab_keyboard(true, win, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .unwrap()
        .reply()
        .unwrap();
    if reply.status == GrabStatus::SUCCESS {
        other.ungrab_keyboard(CURRENT_TIME).unwrap();
        other.flush().unwrap();
        true
    } else {
        false
    }
}

#[test]
fn the_grab_is_real_exclusive_and_released_on_request() {
    let _serial = SERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((_owner_conn, win)) = window() else {
        eprintln!("no X server: skipping");
        return;
    };
    let Some((rival, rival_win)) = window() else {
        return;
    };
    assert!(
        another_client_can_grab(&rival, rival_win),
        "nothing is grabbed yet"
    );

    let mut grab = KeyboardGrab::x11(win).unwrap();
    assert_eq!(grab.backend_name(), "x11");
    assert_eq!(grab.poll(), GrabState::Released, "nothing asked yet");
    grab.set_wanted(true);
    assert_eq!(grab.poll(), GrabState::Active, "an X11 grab is immediate");
    assert!(
        !another_client_can_grab(&rival, rival_win),
        "the server must refuse a second grab"
    );

    // Asking again changes nothing.
    grab.set_wanted(true);
    assert_eq!(grab.poll(), GrabState::Active);

    grab.set_wanted(false);
    assert_eq!(grab.poll(), GrabState::Released);
    assert!(
        another_client_can_grab(&rival, rival_win),
        "released: the keyboard is free again"
    );
}

#[test]
fn dropping_the_grab_gives_the_keyboard_back() {
    let _serial = SERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((_c, win)) = window() else { return };
    let Some((rival, rival_win)) = window() else {
        return;
    };
    {
        let mut grab = KeyboardGrab::x11(win).unwrap();
        grab.set_wanted(true);
        assert_eq!(grab.poll(), GrabState::Active);
        assert!(!another_client_can_grab(&rival, rival_win));
    }
    assert!(another_client_can_grab(&rival, rival_win));
}

#[test]
fn a_nonexistent_window_fails_with_a_reason_instead_of_pretending() {
    let _serial = SERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if RustConnection::connect(None).is_err() {
        eprintln!("no X server: skipping");
        return;
    }
    let mut grab = KeyboardGrab::x11(0x00DE_AD00).unwrap();
    grab.set_wanted(true);
    let state = grab.poll();
    assert!(matches!(state, GrabState::Failed(_)), "{state:?}");
    // The failure does not poison later requests.
    grab.set_wanted(false);
    assert_eq!(grab.poll(), GrabState::Released);
}
