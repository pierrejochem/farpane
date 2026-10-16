//! Pixel encoders and decoders for RFB rectangles.
//!
//! Receives complete rectangle payloads from `rfb-proto`'s framers and turns
//! them into pixels (viewer) or the reverse (server). Each decoder owns its
//! persistent zlib state for the life of the connection.
//!
//! Status: not started. Milestone M2 adds Raw, `CopyRect`, RRE, Hextile, zlib,
//! TRLE and ZRLE; M3 adds Tight. See `docs/design/farpane-reference.md`,
//! sections 7, 15 and 22.

pub use rfb_proto::consts::enc;
