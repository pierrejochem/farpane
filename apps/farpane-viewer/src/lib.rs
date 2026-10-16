//! Farpane Viewer core: connect to a VNC-compatible server and keep a decoded
//! picture of its screen. The window (egui) sits on top of [`RemoteScreen`].

pub mod core;
pub mod input;
mod screen;
mod screenshot;

pub use screen::{ConnectError, DEFAULT_ENCODINGS, RemoteScreen, ScreenEvent};
pub use screenshot::{parse_encodings, write_image};
