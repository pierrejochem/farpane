//! X11 backend for `rfb-server`: the screen through `GetImage` with the
//! `Damage` extension for change tracking, and keyboard and pointer input
//! through `XTest`.
//!
//! Under `XWayland` this sees only X11 clients, not the Wayland desktop; the
//! Wayland backend is a separate piece of work (design doc, section 19).
//!
//! Clipboard: text other programs copy (the `CLIPBOARD` selection, as
//! `UTF8_STRING`) is reported by the source, and text clients copy is served
//! to pasting programs. `PRIMARY` (middle-click) and chunked `INCR` transfers
//! are not handled.

mod input;
mod source;

use std::io;

pub use input::X11Input;
pub use source::X11Source;
use x11rb::{connect, rust_connection::RustConnection};

pub(crate) fn io_err(e: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(e)
}

/// Open `display` (default `$DISPLAY`): one connection for capture and one
/// for input, so neither waits for the other.
///
/// # Errors
/// The display cannot be opened, or lacks `Damage` or `XTest`, or its root
/// window does not use 32 bits per pixel in B, G, R order.
pub fn open(display: Option<&str>) -> io::Result<(X11Source, X11Input)> {
    let (capture, screen) = connect(display).map_err(io_err)?;
    let (input, _) = connect(display).map_err(io_err)?;
    let (to_source, from_input) = std::sync::mpsc::channel();
    Ok((
        X11Source::new(capture, screen, from_input)?,
        X11Input::new(input, screen, to_source)?,
    ))
}

pub(crate) type Conn = RustConnection;
