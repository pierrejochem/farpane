//! X11: `XGrabKeyboard` on the window, from a connection of our own.

use x11rb::{
    CURRENT_TIME,
    protocol::xproto::{ConnectionExt, GrabMode, GrabStatus},
    rust_connection::RustConnection,
};

use crate::{Backend, GrabError, GrabState};

pub(crate) struct X11Grab {
    conn: RustConnection,
    window: u32,
    /// The outcome of the last grab request.
    state: GrabState,
}

impl X11Grab {
    pub(crate) fn new(window: u32) -> Result<Self, GrabError> {
        let (conn, _) =
            RustConnection::connect(None).map_err(|e| GrabError::Connection(e.to_string()))?;
        Ok(Self {
            conn,
            window,
            state: GrabState::Released,
        })
    }
}

impl Backend for X11Grab {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn engage(&mut self) -> GrabState {
        // `owner_events`: key events still arrive at our own window as usual.
        let reply = self
            .conn
            .grab_keyboard(
                true,
                self.window,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|e| e.to_string())
            .and_then(|cookie| cookie.reply().map_err(|e| e.to_string()));
        self.state = match reply {
            Ok(r) if r.status == GrabStatus::SUCCESS => GrabState::Active,
            Ok(r) => GrabState::Failed(format!("the X server refused the grab: {:?}", r.status)),
            Err(e) => GrabState::Failed(e),
        };
        self.state.clone()
    }

    fn release(&mut self) {
        let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
        // A round trip: when this returns the server has processed the ungrab,
        // so another client asking right afterwards is not refused by a grab
        // that is still queued to end.
        let _ = self
            .conn
            .get_input_focus()
            .map(x11rb::cookie::Cookie::reply);
        self.state = GrabState::Released;
    }

    fn poll(&mut self) -> GrabState {
        // An X11 grab is synchronous: whatever `engage` found stays true until
        // release. (Another client taking the keyboard is not announced.)
        self.state.clone()
    }
}
