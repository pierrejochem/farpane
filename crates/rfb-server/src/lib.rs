//! Async RFB server: one capture thread publishing damage, one task per
//! client that owns its pixel format, encoder state and pending changes
//! (design doc, section 23).
//!
//! Platform specifics are behind [`Source`] and [`Input`]. Authentication,
//! TLS and the handshake come from `rfb-tokio`'s [`ServerOptions`].

mod client;
mod region;
mod server;
mod source;

pub use region::Region;
pub use server::{Server, Settings};
pub use source::{Capture, Input, NoInput, Source, TestPattern};
