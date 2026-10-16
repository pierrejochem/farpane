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
//! Status: Raw, `CopyRect`, RRE, `CoRRE`, Hextile, zlib, ZRLE and Tight
//! (without JPEG) decode, plus the `Cursor` and `DesktopSize`
//! pseudo-encodings. TRLE follows in M2. See `docs/design/farpane-reference.md`,
//! sections 7, 15 and 22.

pub mod clipboard;
mod decoder;
mod error;
mod framebuffer;
mod hextile;
mod inflate;
mod pixel;
mod rre;
mod tight;
mod zrle;

pub use decoder::{Cursor, Damage, Decoder, DesktopLayout};
pub use error::DecodeError;
pub use framebuffer::Framebuffer;
pub use pixel::PixelConverter;
pub use rfb_proto::consts::enc;
