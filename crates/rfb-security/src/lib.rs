//! The cryptography behind RFB's security types, without any I/O: the
//! handshake state machines live in `rfb-proto` and `rfb-tokio` runs them.
//!
//! - [`vnc_auth`]: VNC Auth's DES challenge response (bit-reversed key).
//! - [`tls`]: the trust policy for `VeNCrypt` (pinned fingerprints, public or
//!   supplied CAs) as a rustls certificate verifier.
//! - [`ra2`]: RSA-AES (`RA2`, `RA2_256` and the `ne` variants): RSA keys, key
//!   derivation and the AES-EAX message layer.
//!
//! Not implemented: SASL, Apple DH, MS-Logon II, the two-step RSA-AES types.

pub mod ra2;
pub mod tls;
pub mod vnc_auth;
