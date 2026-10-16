#![cfg(all(unix, not(target_vendor = "apple")))]
//! The Wayland backend against the running compositor; skipped without one.
//!
//! A real toplevel surface is created with a small client of its own, then the
//! backend is pointed at that surface through raw pointers, exactly as it is
//! with a winit window. What this shows: our objects are bound and our requests
//! are well formed (a bad id or request makes the compositor drop the
//! connection with a protocol error). What it cannot show: that the shortcuts
//! really reach the window, which needs a focused window and a person pressing
//! keys.

use std::ffi::c_void;

use farpane_grab::{GrabState, KeyboardGrab};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_surface::WlSurface},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

#[derive(Default)]
struct Client {
    configured: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for Client {
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
impl Dispatch<WlCompositor, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: <WlCompositor as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WlSurface, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: <WlSurface as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<XdgToplevel, ()> for Client {
    fn event(
        _: &mut Self,
        _: &XdgToplevel,
        _: <XdgToplevel as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        state: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

/// A connected client with one toplevel window, or `None` without a compositor.
struct Window {
    conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
    surface: WlSurface,
    _toplevel: XdgToplevel,
    _xdg_surface: XdgSurface,
    _base: XdgWmBase,
}

fn window() -> Option<Window> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Client>(&conn).ok()?;
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).ok()?;
    let base: XdgWmBase = globals.bind(&qh, 1..=1, ()).ok()?;
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("farpane-grab test".into());
    surface.commit();
    let mut client = Client::default();
    for _ in 0..20 {
        queue.roundtrip(&mut client).ok()?;
        if client.configured {
            break;
        }
    }
    Some(Window {
        conn,
        queue,
        client,
        surface,
        _toplevel: toplevel,
        _xdg_surface: xdg_surface,
        _base: base,
    })
}

impl Window {
    fn pointers(&self) -> (*mut c_void, *mut c_void) {
        (
            self.conn.backend().display_ptr().cast(),
            self.surface.id().as_ptr().cast(),
        )
    }

    fn grab(&self) -> KeyboardGrab {
        let (display, surface) = self.pointers();
        // SAFETY: both pointers come from this live connection and surface,
        // which outlive every grab made in these tests.
        unsafe { KeyboardGrab::wayland(display, surface) }
            .expect("the compositor offers the protocol")
    }

    /// Round trip so the compositor has processed everything sent so far; an
    /// error here is a protocol error from the compositor.
    fn sync(&mut self) -> Result<(), wayland_client::DispatchError> {
        self.queue.roundtrip(&mut self.client).map(|_| ())
    }
}

#[test]
fn the_compositor_accepts_the_request_and_release_makes_room_for_a_new_one() {
    let Some(mut win) = window() else {
        eprintln!("no Wayland compositor: skipping");
        return;
    };
    let mut first = win.grab();
    assert_eq!(first.backend_name(), "wayland");
    assert_eq!(first.poll(), GrabState::Released);
    first.set_wanted(true);
    // An unfocused test window cannot be told "active", but the request is
    // accepted: still pending, not failed.
    assert!(matches!(
        first.poll(),
        GrabState::Requested | GrabState::Active
    ));
    win.sync().expect("the compositor accepted the inhibitor");

    // Releasing destroys it, so asking again for the same surface and seat is
    // fine. (A compositor that enforces `already_inhibited` would reject the
    // second request if release did nothing; GNOME's mutter does not enforce
    // it, so this part checks only that nothing breaks.)
    first.set_wanted(false);
    win.sync().unwrap();
    let mut second = win.grab();
    second.set_wanted(true);
    win.sync()
        .expect("a new inhibitor after the first one was released");
    drop(second);
    win.sync().unwrap();
}
