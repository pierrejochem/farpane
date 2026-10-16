//! Keyboard capture for a remote desktop window.
//!
//! While you look at a remote machine, shortcuts such as Alt+Tab, the Super
//! key or Ctrl+Alt+Del belong to *that* machine, but the local desktop grabs
//! them first. This crate asks the local windowing system to hand them to the
//! window instead:
//!
//! - **Wayland**: the `keyboard-shortcuts-inhibit` protocol
//!   (`zwp_keyboard_shortcuts_inhibit_manager_v1`). The compositor may still
//!   reserve a way out for the user and tells the client when the inhibition
//!   is really active.
//! - **X11**: `XGrabKeyboard` on the window, through Xlib on winit's own
//!   display (a grab is delivered to the grabbing client, so it must be the
//!   window's own).
//!
//! Windows (a low-level keyboard hook) and macOS (an event tap that needs the
//! Accessibility permission) are not implemented: they cannot be developed
//! and tested here, and untested input-hook code is worse than none.
//! [`KeyboardGrab::from_window`] reports them as unsupported.
//!
//! # Why this crate has `unsafe`
//!
//! The workspace denies `unsafe`. The Wayland backend needs it in two places,
//! both because winit owns the connection and the window and only hands out
//! raw pointers (through `raw-window-handle`):
//!
//! 1. `Backend::from_foreign_display` wraps winit's `wl_display` so our
//!    protocol objects live on the same connection as the window's surface;
//! 2. `ObjectId::from_ptr` turns winit's `wl_surface` pointer into a proxy.
//!
//! and the X11 backend calls Xlib on winit's `Display*` (`XGrabKeyboard`,
//! `XUngrabKeyboard`, `XSync`, `XFlush`) because the grab must be made by the
//! window's own client.
//!
//! All of it is only sound while the window is alive, which is why the unsafe
//! constructors document that contract and [`KeyboardGrab`] must be dropped
//! before the window, and used from the event loop thread. Each `unsafe`
//! block has a `SAFETY` comment. Nothing else in the crate uses `unsafe`.

#[cfg(all(unix, not(target_vendor = "apple")))]
mod wayland;
#[cfg(all(unix, not(target_vendor = "apple")))]
mod x11;

#[cfg(all(unix, not(target_vendor = "apple")))]
use std::ffi::c_void;

#[cfg(all(unix, not(target_vendor = "apple")))]
use raw_window_handle::RawDisplayHandle;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};

/// Where a capture request stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrabState {
    /// Not asked for, or released again.
    Released,
    /// Asked for; the windowing system has not confirmed yet (Wayland only
    /// confirms once the window has keyboard focus).
    Requested,
    /// Shortcuts and system keys now go to the window.
    Active,
    /// The request failed or was withdrawn by the system.
    Failed(String),
}

/// Why a [`KeyboardGrab`] could not be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrabError {
    /// This platform or compositor has no way to do it.
    Unsupported(String),
    /// Connecting to the windowing system failed.
    Connection(String),
}

impl std::fmt::Display for GrabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(why) => write!(f, "keyboard capture is not available: {why}"),
            Self::Connection(why) => write!(f, "cannot reach the windowing system: {why}"),
        }
    }
}

impl std::error::Error for GrabError {}

/// One platform's way of capturing the keyboard.
trait Backend {
    fn name(&self) -> &'static str;
    /// Ask for the capture.
    fn engage(&mut self) -> GrabState;
    /// Give the keyboard back.
    fn release(&mut self);
    /// Pick up news from the windowing system and report the current state.
    fn poll(&mut self) -> GrabState;
}

/// Capture of the keyboard for one window.
pub struct KeyboardGrab {
    backend: Box<dyn Backend>,
    wanted: bool,
    state: GrabState,
}

impl std::fmt::Debug for KeyboardGrab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyboardGrab")
            .field("backend", &self.backend.name())
            .field("wanted", &self.wanted)
            .field("state", &self.state)
            .finish()
    }
}

impl KeyboardGrab {
    /// Pick the backend for `window` from its raw handles.
    ///
    /// The returned value must be dropped before the window is.
    ///
    /// # Errors
    /// [`GrabError::Unsupported`] on platforms or compositors without a way
    /// to capture the keyboard, [`GrabError::Connection`] if the windowing
    /// system cannot be reached.
    pub fn from_window(
        window: &(impl HasWindowHandle + HasDisplayHandle),
    ) -> Result<Self, GrabError> {
        let handle = window
            .window_handle()
            .map_err(|e| GrabError::Unsupported(e.to_string()))?
            .as_raw();
        let display = window
            .display_handle()
            .map_err(|e| GrabError::Unsupported(e.to_string()))?
            .as_raw();
        match (handle, display) {
            #[cfg(all(unix, not(target_vendor = "apple")))]
            (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => {
                // SAFETY: the handles come from a live window (`window` is
                // borrowed for this call) and the documented contract of this
                // function is that the result is dropped before the window.
                unsafe { Self::wayland(d.display.as_ptr(), w.surface.as_ptr()) }
            }
            #[cfg(all(unix, not(target_vendor = "apple")))]
            (RawWindowHandle::Xlib(w), RawDisplayHandle::Xlib(d)) => {
                let display = d
                    .display
                    .ok_or_else(|| GrabError::Connection("no X display".into()))?;
                // SAFETY: as above: live window, result dropped before it.
                unsafe { Self::x11(display.as_ptr(), w.window) }
            }
            (RawWindowHandle::Xcb(_), _) => Err(GrabError::Unsupported(
                "windows on an xcb connection are not supported (winit uses Xlib)".into(),
            )),
            (other, _) => Err(GrabError::Unsupported(format!(
                "no backend for this window system ({other:?})"
            ))),
        }
    }

    #[cfg(all(unix, not(target_vendor = "apple")))]
    /// A Wayland capture for the surface `surface` on the connection
    /// `display` (the `wl_display` and `wl_surface` pointers winit exposes).
    ///
    /// # Errors
    /// As [`KeyboardGrab::from_window`].
    ///
    /// # Safety
    /// Both pointers must be valid, belong together, and stay valid for as
    /// long as the returned value lives.
    pub unsafe fn wayland(display: *mut c_void, surface: *mut c_void) -> Result<Self, GrabError> {
        // SAFETY: forwarded from this function's contract.
        let backend = unsafe { wayland::WaylandGrab::new(display, surface)? };
        Ok(Self::with(Box::new(backend)))
    }

    #[cfg(all(unix, not(target_vendor = "apple")))]
    /// An X11 capture for `window` on the Xlib display `display`.
    ///
    /// # Errors
    /// [`GrabError::Connection`] if libX11 cannot be loaded.
    ///
    /// # Safety
    /// `display` must be a valid `Display*` and `window` a window of it, both
    /// valid for as long as the returned value lives, and the value must be
    /// used only from the thread that owns the display.
    pub unsafe fn x11(display: *mut c_void, window: std::ffi::c_ulong) -> Result<Self, GrabError> {
        // SAFETY: forwarded from this function's contract.
        let backend = unsafe { x11::X11Grab::new(display, window)? };
        Ok(Self::with(Box::new(backend)))
    }

    // Only the Linux/BSD backends construct one; elsewhere nothing does.
    #[cfg_attr(not(all(unix, not(target_vendor = "apple"))), allow(dead_code))]
    fn with(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            wanted: false,
            state: GrabState::Released,
        }
    }

    /// Which backend is in use (`"wayland"` or `"x11"`).
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Ask for the capture (`true`) or give the keyboard back (`false`).
    /// Asking again while already asked changes nothing.
    pub fn set_wanted(&mut self, wanted: bool) {
        if wanted == self.wanted {
            return;
        }
        self.wanted = wanted;
        if wanted {
            self.state = self.backend.engage();
        } else {
            self.backend.release();
            self.state = GrabState::Released;
        }
    }

    /// Whether the capture is currently asked for.
    #[must_use]
    pub fn wanted(&self) -> bool {
        self.wanted
    }

    /// Process pending news from the windowing system and return the state.
    /// Call once per frame.
    pub fn poll(&mut self) -> GrabState {
        if self.wanted {
            self.state = self.backend.poll();
        }
        self.state.clone()
    }
}

impl Drop for KeyboardGrab {
    fn drop(&mut self) {
        if self.wanted {
            self.backend.release();
        }
    }
}
