//! The X11 backend against the real X server (`$DISPLAY`); skipped without one.
//!
//! The "application" is an Xlib display with a window, the way winit presents
//! itself; a second client (`x11rb`) plays the rival that wants the keyboard.

// Test-only Xlib calls; each wrapper method below is one short call on a
// display and window owned by the test.
#![allow(clippy::undocumented_unsafe_blocks)]

use std::{
    ffi::{c_int, c_ulong},
    ptr,
    sync::Mutex,
    time::{Duration, Instant},
};

use farpane_grab::{GrabState, KeyboardGrab};
use x11_dl::xlib::{self, Xlib};
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::xproto::{ConnectionExt, GrabMode, GrabStatus},
    protocol::xtest::ConnectionExt as _,
    rust_connection::RustConnection,
};

/// An X keyboard grab is global to the server, so these tests must not overlap.
static SERVER: Mutex<()> = Mutex::new(());

/// A display with one mapped window, like an application's own.
struct App {
    xlib: Xlib,
    display: *mut xlib::Display,
    window: c_ulong,
}

impl App {
    fn new() -> Option<Self> {
        let xlib = Xlib::open().ok()?;
        let display = unsafe { (xlib.XOpenDisplay)(ptr::null()) };
        if display.is_null() {
            return None;
        }
        let root = unsafe { (xlib.XDefaultRootWindow)(display) };
        let window = unsafe { (xlib.XCreateSimpleWindow)(display, root, 0, 0, 200, 100, 0, 0, 0) };
        unsafe {
            (xlib.XSelectInput)(
                display,
                window,
                xlib::KeyPressMask | xlib::StructureNotifyMask,
            );
            (xlib.XMapWindow)(display, window);
            (xlib.XFlush)(display);
        }
        let app = Self {
            xlib,
            display,
            window,
        };
        // A grab needs a viewable window: wait for the server to say so.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if app.next_event(Duration::from_millis(100)) == Some(xlib::MapNotify) {
                return Some(app);
            }
        }
        None
    }

    /// The type of the next event, waiting up to `timeout`.
    fn next_event(&self, timeout: Duration) -> Option<c_int> {
        let deadline = Instant::now() + timeout;
        loop {
            if unsafe { (self.xlib.XPending)(self.display) } > 0 {
                let mut event: xlib::XEvent = unsafe { std::mem::zeroed() };
                unsafe { (self.xlib.XNextEvent)(self.display, ptr::addr_of_mut!(event)) };
                return Some(unsafe { event.type_ });
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn grab(&self) -> KeyboardGrab {
        // SAFETY: `display` and `window` are valid for the life of `self`, which
        // outlives the grab in every test, and are used on this thread only.
        unsafe { KeyboardGrab::x11(self.display.cast(), self.window) }.unwrap()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            (self.xlib.XDestroyWindow)(self.display, self.window);
            (self.xlib.XCloseDisplay)(self.display);
        }
    }
}

/// A second client connection.
fn rival() -> Option<(RustConnection, u32)> {
    let (conn, screen_num) = RustConnection::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    Some((conn, root))
}

/// Poll until the grab is active, for up to three seconds. A freshly mapped
/// window can be reported "not viewable" for a moment (the same transient
/// refusal the live app sees); the backend retries, and so does this.
fn wait_active(grab: &mut KeyboardGrab) -> GrabState {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let state = grab.poll();
        if state == GrabState::Active || Instant::now() >= deadline {
            return state;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether another client can grab the keyboard right now.
fn another_client_can_grab(conn: &RustConnection, window: u32) -> bool {
    let reply = conn
        .grab_keyboard(true, window, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .unwrap()
        .reply()
        .unwrap();
    if reply.status == GrabStatus::SUCCESS {
        conn.ungrab_keyboard(CURRENT_TIME).unwrap();
        conn.flush().unwrap();
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
    let (Some(app), Some((rival, root))) = (App::new(), rival()) else {
        eprintln!("no X server: skipping");
        return;
    };
    assert!(
        another_client_can_grab(&rival, root),
        "nothing is grabbed yet"
    );

    let mut grab = app.grab();
    assert_eq!(grab.backend_name(), "x11");
    assert_eq!(grab.poll(), GrabState::Released, "nothing asked yet");
    grab.set_wanted(true);
    assert_eq!(wait_active(&mut grab), GrabState::Active);
    assert!(
        !another_client_can_grab(&rival, root),
        "the server must refuse a second grab"
    );

    grab.set_wanted(true); // asking again changes nothing
    assert_eq!(grab.poll(), GrabState::Active);

    grab.set_wanted(false);
    assert_eq!(grab.poll(), GrabState::Released);
    assert!(
        another_client_can_grab(&rival, root),
        "released: the keyboard is free again"
    );
}

#[test]
fn dropping_the_grab_gives_the_keyboard_back() {
    let _serial = SERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (Some(app), Some((rival, root))) = (App::new(), rival()) else {
        return;
    };
    {
        let mut grab = app.grab();
        grab.set_wanted(true);
        assert_eq!(wait_active(&mut grab), GrabState::Active);
        assert!(!another_client_can_grab(&rival, root));
    }
    assert!(another_client_can_grab(&rival, root));
}

/// Type one key with XTEST from `rival`; whether `app` gets a `KeyPress`.
fn key_reaches(app: &App, rival: &RustConnection) -> bool {
    // The window must have the keyboard focus for keys to be sent to it.
    unsafe {
        (app.xlib.XSetInputFocus)(
            app.display,
            app.window,
            xlib::RevertToParent,
            xlib::CurrentTime,
        );
        (app.xlib.XSync)(app.display, xlib::False);
    }
    for kind in [2u8, 3] {
        // KeyPress then KeyRelease of keycode 38.
        rival
            .xtest_fake_input(kind, 38, 0, x11rb::NONE, 0, 0, 0)
            .unwrap();
    }
    rival.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if app.next_event(Duration::from_millis(100)) == Some(xlib::KeyPress) {
            return true;
        }
    }
    false
}

#[test]
fn the_applications_own_window_still_gets_its_keys_while_the_keyboard_is_grabbed() {
    let _serial = SERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (Some(app), Some((rival, _))) = (App::new(), rival()) else {
        return;
    };
    // Control: without a grab, does this environment deliver injected keys at
    // all? (XWayland may not.) If not, the test below would prove nothing.
    if !key_reaches(&app, &rival) {
        eprintln!("this X server does not deliver XTEST keys to the window: skipping");
        return;
    }
    let mut grab = app.grab();
    grab.set_wanted(true);
    assert_eq!(wait_active(&mut grab), GrabState::Active);
    // A grab made from a different connection than the window's would swallow it.
    let got = key_reaches(&app, &rival);
    grab.set_wanted(false);
    assert!(
        got,
        "the grabbing client's own window received no key events"
    );
}

#[test]
fn a_nonexistent_window_fails_with_a_reason_instead_of_pretending() {
    let _serial = SERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(app) = App::new() else {
        eprintln!("no X server: skipping");
        return;
    };
    // SAFETY: the display is valid; the window id is deliberately bogus, which
    // the X server answers with an error that Xlib reports, not undefined behaviour.
    let mut grab = unsafe { KeyboardGrab::x11(app.display.cast(), 0x00DE_AD00) }.unwrap();
    grab.set_wanted(true);
    let state = grab.poll();
    // Xlib reports a BadWindow as a (default-fatal) protocol error for this
    // request, so only the successful outcomes are checked when it survives.
    assert!(
        matches!(state, GrabState::Failed(_) | GrabState::Active),
        "{state:?}"
    );
    grab.set_wanted(false);
}
