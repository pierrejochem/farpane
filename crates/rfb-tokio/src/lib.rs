//! Tokio adapter for `rfb-proto`: async [`ClientSession`] and [`ServerSession`]
//! over any `AsyncRead + AsyncWrite` stream (TCP today; TLS and WebSocket
//! streams plug in the same way).
//!
//! The sessions do only I/O. They own a read buffer, feed it to the sans-IO
//! connection from `rfb-proto`, write what it produces, enforce the handshake
//! timeout and answer VNC Auth through `rfb-security`. Policy (encoder choice,
//! damage tracking, UI) lives above.
//!
//! # Cancel safety
//! `next_event` and `next_message` can be dropped from a `select!` without
//! losing received bytes. Writes are different: a `send*` call that is
//! cancelled part-way may leave half a message on the wire, so await sends to
//! completion.

mod client;
mod error;
mod io;
mod server;
mod transport;

pub use client::{ClientOptions, ClientSession, ServerInfo, TlsOptions};
pub use error::Error;
pub use server::{PlainVerifier, ServerAuth, ServerOptions, ServerSession};
pub use transport::Transport;

/// How long a handshake may take before the session gives up (design doc,
/// section 15, "Limits").
pub const HANDSHAKE_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(30);
