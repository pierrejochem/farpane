//! X11: `XGrabKeyboard` on the window, through Xlib on winit's own display.
//!
//! It has to be winit's connection: an X keyboard grab delivers key events to
//! the *grabbing client*, so a grab made from a second connection would leave
//! the application's own window without any keys. With `owner_events` set and
//! the grab made by the window's own client, key events keep arriving at the
//! window as usual while system shortcuts stop being intercepted.

use std::{
    ffi::{c_int, c_ulong, c_void},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use x11_dl::xlib::{self, Xlib};

use crate::{Backend, GrabError, GrabState};

/// How long to wait before asking again after a refusal. The X server refuses
/// transiently (the window is not mapped yet, another client holds a grab for a
/// moment), and the user should not have to toggle anything.
const RETRY_AFTER: Duration = Duration::from_millis(500);

/// Set by [`record_error`] while a grab request is in flight.
static REQUEST_FAILED: AtomicBool = AtomicBool::new(false);

/// A temporary Xlib error handler: remember the error instead of letting
/// Xlib's default handler exit the whole process (a `BadWindow` for a window
/// that just went away must not take the viewer down with it).
unsafe extern "C" fn record_error(_: *mut xlib::Display, _: *mut xlib::XErrorEvent) -> c_int {
    REQUEST_FAILED.store(true, Ordering::SeqCst);
    0
}

pub(crate) struct X11Grab {
    xlib: Xlib,
    display: *mut xlib::Display,
    window: c_ulong,
    state: GrabState,
    asked_at: Option<Instant>,
}

impl X11Grab {
    /// # Safety
    /// `display` is winit's `Display*` and `window` a window of that display;
    /// both stay valid for the life of the returned value, and it is used only
    /// from the thread that owns the display (the event loop thread).
    pub(crate) unsafe fn new(display: *mut c_void, window: c_ulong) -> Result<Self, GrabError> {
        let xlib = Xlib::open().map_err(|e| GrabError::Connection(e.to_string()))?;
        if display.is_null() {
            return Err(GrabError::Connection("no X display".into()));
        }
        Ok(Self {
            xlib,
            display: display.cast(),
            window,
            state: GrabState::Released,
            asked_at: None,
        })
    }

    fn grab(&mut self) -> GrabState {
        self.asked_at = Some(Instant::now());
        REQUEST_FAILED.store(false, Ordering::SeqCst);
        // SAFETY: `display` and `window` are valid for the life of `self` and
        // used on the owning thread (this type's constructor contract). The
        // error handler is restored right after the request, and Xlib handlers
        // are process-wide, which is fine because this runs on the single
        // thread that owns the display.
        let (status, previous) = unsafe {
            let previous = (self.xlib.XSetErrorHandler)(Some(record_error));
            let status = (self.xlib.XGrabKeyboard)(
                self.display,
                self.window,
                xlib::True,
                xlib::GrabModeAsync,
                xlib::GrabModeAsync,
                xlib::CurrentTime,
            );
            // Make any error for this request arrive while our handler is installed.
            (self.xlib.XSync)(self.display, xlib::False);
            (self.xlib.XSetErrorHandler)(previous);
            (status, previous)
        };
        let _ = previous;
        if REQUEST_FAILED.load(Ordering::SeqCst) {
            return GrabState::Failed(
                "the X server rejected the grab request (bad window?)".into(),
            );
        }
        match status {
            xlib::GrabSuccess => GrabState::Active,
            other => GrabState::Failed(format!(
                "the X server refused the grab: {}",
                status_name(other)
            )),
        }
    }
}

fn status_name(status: c_int) -> &'static str {
    match status {
        xlib::AlreadyGrabbed => "another client holds the keyboard",
        xlib::GrabInvalidTime => "invalid time",
        xlib::GrabNotViewable => "the window is not viewable",
        xlib::GrabFrozen => "the keyboard is frozen by another grab",
        _ => "unknown reason",
    }
}

impl Backend for X11Grab {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn engage(&mut self) -> GrabState {
        self.state = self.grab();
        self.state.clone()
    }

    fn release(&mut self) {
        // SAFETY: valid display, owning thread (constructor contract).
        unsafe {
            (self.xlib.XUngrabKeyboard)(self.display, xlib::CurrentTime);
            // A round trip: when this returns the server has processed the
            // ungrab, so another client asking right afterwards is not refused
            // by a grab that is still queued to end.
            (self.xlib.XSync)(self.display, xlib::False);
        }
        self.state = GrabState::Released;
        self.asked_at = None;
    }

    fn poll(&mut self) -> GrabState {
        // A refusal is often temporary: ask again from time to time.
        if matches!(self.state, GrabState::Failed(_))
            && self.asked_at.is_none_or(|t| t.elapsed() >= RETRY_AFTER)
        {
            self.state = self.grab();
        }
        self.state.clone()
    }
}
