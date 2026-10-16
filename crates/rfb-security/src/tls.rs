//! Certificate trust for `VeNCrypt` TLS.
//!
//! VNC servers mostly use self-signed certificates, so the usual "is it signed
//! by a public CA" check would reject most of them. The policy here is the
//! one SSH uses for host keys:
//!
//! 1. a certificate whose SHA-256 fingerprint the user has *pinned* for this
//!    server is accepted (even when self-signed or expired: the user chose
//!    exactly this certificate);
//! 2. otherwise a certificate that a public CA signed for this name is
//!    accepted;
//! 3. otherwise the handshake fails and the verifier records what it saw, so
//!    the caller can ask the user. A server that already has a pin but now
//!    shows a different certificate is reported as *changed*, which is the
//!    case that deserves a loud warning.
//!
//! The verifier cannot ask anyone itself (it runs inside the TLS handshake,
//! synchronously), hence the record-fail-ask-retry shape.

use std::sync::{Arc, Mutex};

use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::{CryptoProvider, ring},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};

/// A SHA-256 fingerprint of a certificate's DER encoding.
pub type Fingerprint = [u8; 32];

/// The fingerprint of `cert_der`.
#[must_use]
pub fn fingerprint(cert_der: &[u8]) -> Fingerprint {
    Sha256::digest(cert_der).into()
}

/// `AB:CD:...` with uppercase hex, the way fingerprints are usually shown.
#[must_use]
pub fn format_fingerprint(fp: &Fingerprint) -> String {
    fp.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Parse `format_fingerprint`'s output (colons optional, any case).
#[must_use]
pub fn parse_fingerprint(text: &str) -> Option<Fingerprint> {
    let hex: String = text
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect();
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut fp = [0u8; 32];
    for (i, byte) in fp.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(fp)
}

/// Why a certificate was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertProblem {
    /// Never seen for this server and not signed by a public CA: a new
    /// server, or a self-signed one. The user may pin it.
    Unknown,
    /// This server has a pinned certificate and now shows a different one:
    /// possibly an attack, possibly a renewed certificate.
    Changed,
    /// Expired, not valid for this name, or otherwise broken, and not pinned.
    Invalid(String),
}

/// What the verifier saw when it refused a certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untrusted {
    pub fingerprint: Fingerprint,
    pub problem: CertProblem,
}

/// Where the verifier leaves its finding for the caller to read.
pub type Finding = Arc<Mutex<Option<Untrusted>>>;

#[derive(Debug)]
struct Verifier {
    webpki: Arc<WebPkiServerVerifier>,
    pins: Vec<Fingerprint>,
    finding: Finding,
}

impl Verifier {
    fn refuse(&self, fingerprint: Fingerprint, problem: CertProblem, error: Error) -> Error {
        if let Ok(mut slot) = self.finding.lock() {
            *slot = Some(Untrusted {
                fingerprint,
                problem,
            });
        }
        error
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let fp = fingerprint(end_entity.as_ref());
        if self.pins.contains(&fp) {
            return Ok(ServerCertVerified::assertion());
        }
        match self.webpki.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(ok) => Ok(ok),
            Err(error) => {
                let problem = if !self.pins.is_empty() {
                    CertProblem::Changed
                } else if matches!(
                    &error,
                    Error::InvalidCertificate(CertificateError::UnknownIssuer)
                ) || error.to_string().contains("CaUsedAsEndEntity")
                {
                    // A self-signed certificate that is flagged as a CA (many
                    // servers make theirs that way) is still just a
                    // self-signed one: the user may pin it.
                    CertProblem::Unknown
                } else {
                    CertProblem::Invalid(error.to_string())
                };
                Err(self.refuse(fp, problem, error))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

/// A TLS client configuration for connecting to one server.
///
/// `pins` are the fingerprints the user trusts for this server (empty for a
/// server never seen). `extra_roots` are additional CA certificates to
/// trust besides the built-in public ones (for a private CA). After a failed
/// handshake, read the returned [`Finding`].
///
/// # Errors
/// A message if a root certificate is unusable.
pub fn client_config(
    pins: Vec<Fingerprint>,
    extra_roots: &[Vec<u8>],
) -> Result<(Arc<ClientConfig>, Finding), String> {
    let provider: Arc<CryptoProvider> = Arc::new(ring::default_provider());
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for der in extra_roots {
        roots
            .add(CertificateDer::from(der.clone()))
            .map_err(|e| format!("unusable CA certificate: {e}"))?;
    }
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|e| format!("cannot build the certificate verifier: {e}"))?;
    let finding: Finding = Arc::new(Mutex::new(None));
    let verifier = Verifier {
        webpki,
        pins,
        finding: finding.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok((Arc::new(config), finding))
}
