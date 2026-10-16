//! Security handshakes for RFB, plugged into `rfb-proto` through one trait.
//!
//! Status: [`vnc_auth`] (DES with bit-reversed key) is done. Planned
//! implementations (design doc section 4 and 20): `VeNCrypt` with rustls,
//! RA2 and RA2-256 (RSA + AES-EAX framing), Apple DH (viewer), checking
//! credentials against the server's own user list (Argon2id).

pub mod vnc_auth;

/// What a security step asks the I/O layer to do next.
#[derive(Debug)]
pub enum StepOutput {
    /// Send these bytes to the peer.
    Send(Vec<u8>),
    /// Upgrade the transport to TLS now (`VeNCrypt`), then continue.
    StartTls,
    /// From now on, wrap the stream in the negotiated cipher (RA2).
    InstallCipher,
    /// The handshake finished successfully.
    Done,
}

/// One security type's handshake, driven by incoming bytes.
pub trait SecurityStep: std::fmt::Debug + Send {
    /// The RFB security type number (see `rfb_proto::consts::security`).
    fn security_type(&self) -> u8;

    /// Feed received bytes; returns what to do next.
    ///
    /// # Errors
    /// Returns a protocol error if the peer's data is invalid or
    /// authentication fails.
    fn on_input(&mut self, input: &[u8]) -> Result<StepOutput, rfb_proto::ProtoError>;
}
