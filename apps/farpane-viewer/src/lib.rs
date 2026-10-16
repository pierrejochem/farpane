//! Farpane Viewer: connect to a VNC-compatible server and keep a decoded
//! picture of its screen ([`RemoteScreen`]), shown in an egui window
//! ([`app::ViewerApp`]) that talks to the connection thread ([`core::Core`]).

pub mod app;
pub mod appearance;
pub mod clipboard;
pub mod core;
pub mod input;
mod screen;
mod screenshot;
pub mod settings;
pub mod stats;

pub use screen::{
    ConnectError, ConnectOptions, DEFAULT_ENCODINGS, RemoteScreen, ScreenEvent, host_of,
};
pub use screenshot::{parse_encodings, write_image};
