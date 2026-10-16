//! Farpane Viewer: connect to a VNC-compatible server and keep a decoded
//! picture of its screen ([`RemoteScreen`]), shown in an egui window
//! ([`app::ViewerApp`]) that talks to the connection thread ([`core::Core`]).

pub mod app;
pub mod clipboard;
pub mod core;
pub mod input;
mod screen;
mod screenshot;
pub mod settings;

pub use screen::{ConnectError, DEFAULT_ENCODINGS, RemoteScreen, ScreenEvent};
pub use screenshot::{parse_encodings, write_image};
