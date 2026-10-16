//! Wayland: `zwp_keyboard_shortcuts_inhibit_manager_v1`.
//!
//! The protocol objects are created on winit's own connection, so the
//! compositor sees the request come from the window's client. Events are read
//! by winit's loop and queued for our event queue, which `poll` drains.

use std::ffi::c_void;

use wayland_backend::{client::ObjectId, sys::client::Backend};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat, wl_surface::WlSurface},
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};

use crate::{Backend as GrabBackend, GrabError, GrabState};

/// What the compositor told us about the inhibitor.
#[derive(Debug, Default)]
struct State {
    active: bool,
    /// The compositor withdrew the inhibition (for example the user pressed
    /// its escape shortcut) and the inhibitor is no longer useful.
    withdrawn: bool,
}

pub(crate) struct WaylandGrab {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    qh: QueueHandle<State>,
    manager: ZwpKeyboardShortcutsInhibitManagerV1,
    seat: WlSeat,
    surface: WlSurface,
    inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
}

impl WaylandGrab {
    /// # Safety
    /// `display` is winit's `wl_display*` and `surface` its `wl_surface*` of
    /// the window; both stay valid for the life of the returned value.
    pub(crate) unsafe fn new(
        display: *mut c_void,
        surface: *mut c_void,
    ) -> Result<Self, GrabError> {
        // SAFETY: `display` is a live `wl_display*` (this function's
        // contract). A foreign backend does not own the display, so dropping
        // our wrapper does not disconnect winit's connection.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);

        // SAFETY: `surface` is a live `wl_surface*` on that display (this
        // function's contract).
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }
            .map_err(|e| GrabError::Connection(format!("bad surface: {e}")))?;
        let surface = WlSurface::from_id(&conn, id)
            .map_err(|e| GrabError::Connection(format!("bad surface: {e}")))?;

        let (globals, queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| GrabError::Connection(e.to_string()))?;
        let qh = queue.handle();
        let manager = globals
            .bind::<ZwpKeyboardShortcutsInhibitManagerV1, _, _>(&qh, 1..=1, ())
            .map_err(|_| {
                GrabError::Unsupported(
                    "the compositor does not offer keyboard-shortcuts-inhibit".into(),
                )
            })?;
        let seat = globals
            .bind::<WlSeat, _, _>(&qh, 1..=1, ())
            .map_err(|e| GrabError::Unsupported(format!("no seat: {e}")))?;
        Ok(Self {
            conn,
            queue,
            state: State::default(),
            qh,
            manager,
            seat,
            surface,
            inhibitor: None,
        })
    }

    fn drain(&mut self) {
        // Events were read from the socket by winit's loop and sorted into our
        // queue; this only runs their handlers and never blocks.
        let _ = self.queue.dispatch_pending(&mut self.state);
    }
}

impl GrabBackend for WaylandGrab {
    fn name(&self) -> &'static str {
        "wayland"
    }

    fn engage(&mut self) -> GrabState {
        if self.inhibitor.is_none() {
            self.state = State::default();
            self.inhibitor =
                Some(
                    self.manager
                        .inhibit_shortcuts(&self.surface, &self.seat, &self.qh, ()),
                );
            let _ = self.conn.flush();
        }
        // Not active until the compositor says so: that happens only once the
        // window has keyboard focus.
        GrabState::Requested
    }

    fn release(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
            let _ = self.conn.flush();
        }
        self.state = State::default();
    }

    fn poll(&mut self) -> GrabState {
        self.drain();
        if self.state.withdrawn {
            GrabState::Failed("the compositor ended the capture".into())
        } else if self.state.active {
            GrabState::Active
        } else {
            GrabState::Requested
        }
    }
}

impl Drop for WaylandGrab {
    fn drop(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
            let _ = self.conn.flush();
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitManagerV1,
        _: <ZwpKeyboardShortcutsInhibitManagerV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => state.active = true,
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                // The compositor ended it (the user used its escape hatch).
                state.active = false;
                state.withdrawn = true;
            }
            _ => {}
        }
    }
}
