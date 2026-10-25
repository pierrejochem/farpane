//! Async RFB server: one capture thread publishing damage, one task per
//! client that owns its pixel format, encoder state and pending changes
//! (design doc, section 23).
//!
//! Platform specifics are behind [`Source`] and [`Input`]. Authentication,
//! TLS and the handshake come from `rfb-tokio`'s `ServerOptions`.

mod client;
mod congestion;
mod region;
mod registry;
mod scroll;
mod server;
mod source;
mod throttle;
mod tiles;

pub use region::Region;
pub use registry::{ClientInfo, Preview};
pub use scroll::{Copy, find_copy, find_copy_sideways};
pub use server::{
    DEFAULT_APPROVAL_TIMEOUT, DEFAULT_COMPRESSION, DEFAULT_MAX_CONNECTIONS, Decision, Event,
    PendingRequest, Permission, Policy, Server, Settings,
};
pub use source::{Capture, Input, NoInput, Source, TestPattern};
pub use throttle::Throttle;
pub use tiles::changed_tiles;
