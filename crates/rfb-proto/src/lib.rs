//! Sans-IO core of the RFB (Remote Framebuffer) protocol used by VNC.
//!
//! This crate turns bytes into typed events and typed messages into bytes, for
//! both the client (viewer) and the server role. It does no I/O, has no async
//! runtime and no crypto, so it is deterministic, easy to fuzz and usable from
//! Tokio, threads or WASM alike. See `docs/design/farpane-reference.md`,
//! section 15, for the full plan.
//!
//! Status: milestone M1 is nearly done. Implemented so far:
//! - [`consts`]: every protocol number, verified against IANA and the
//!   community specification.
//! - [`version`]: the 12-byte `ProtocolVersion` handshake message.
//! - [`types::PixelFormat`]: the 16-byte pixel format structure.
//! - [`wire`]: a bounds-checked big-endian reader.
//! - [`conn`]: `ClientConn` and `ServerConn`, the handshake plus the message loop (M2).
//! - [`framing`]: finds the end of each rectangle payload (M2, in progress).
//! - [`msg`]: the core client and server messages.
//! - [`handshake`]: client and server handshake state machines for 3.3, 3.7
//!   and 3.8 with None and VNC Auth.

#![no_std]

extern crate alloc;

pub mod conn;
pub mod consts;
pub mod error;
pub mod framing;
pub mod handshake;
pub mod msg;
pub mod types;
pub mod version;
pub mod wire;

pub use error::ProtoError;
pub use types::PixelFormat;
pub use version::ProtocolVersion;
