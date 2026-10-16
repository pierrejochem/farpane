//! Pixel encoders and decoders for RFB rectangles.
//!
//! Receives complete rectangle payloads from `rfb-proto`'s framers and turns
//! them into pixels (viewer) or the reverse (server). The [`Decoder`] owns the
//! per-connection state, including (later) the persistent zlib streams.
//!
//! The framebuffer is always 32 bits per pixel in `B, G, R, 0xFF` byte order
//! ([`Framebuffer`]); wire formats are converted on the way in
//! ([`PixelConverter`]).
//!
//! Status: Raw, `CopyRect`, RRE, `CoRRE` and Hextile decode. zlib, ZRLE and
//! TRLE follow in M2, Tight in M3. See `docs/design/farpane-reference.md`,
//! sections 7, 15 and 22.

mod decoder;
mod error;
mod framebuffer;
mod hextile;
mod pixel;
mod rre;

pub use decoder::{Damage, Decoder};
pub use error::DecodeError;
pub use framebuffer::Framebuffer;
pub use pixel::PixelConverter;
pub use rfb_proto::consts::enc;
