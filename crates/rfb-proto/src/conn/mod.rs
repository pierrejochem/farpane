//! Connection state machines: handshake plus the normal message loop.

mod client;
mod server;

pub use client::{ClientConn, ClientConnEvent};
pub use server::{EncodedRect, ServerConn, ServerConnEvent};

#[cfg(test)]
mod tests;
