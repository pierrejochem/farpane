//! Connection state machines: handshake plus the normal message loop.

mod client;

pub use client::{ClientConn, ConnEvent};
