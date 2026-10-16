use core::fmt;

use rfb_proto::ProtoError;
use rfb_security::tls::{CertProblem, Fingerprint, format_fingerprint};

/// Errors from a session. All are fatal to the connection.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Proto(ProtoError),
    /// The peer closed the stream.
    Closed,
    /// The handshake took longer than the configured timeout.
    HandshakeTimeout,
    /// The server wants a password and none was given.
    PasswordRequired,
    /// The password was wrong (server side: the client's response failed).
    AuthFailed,
    /// The server wants a username (`VeNCrypt` `X509Plain`) and none was given.
    UsernameRequired,
    /// The server's TLS certificate (or RSA-AES public key) is not trusted. Show `fingerprint` to the
    /// user; if they accept it, pin it and connect again.
    Untrusted {
        host: String,
        fingerprint: Fingerprint,
        problem: CertProblem,
    },
    /// The TLS handshake failed for another reason.
    Tls(String),
    /// The RSA-AES handshake failed (a bad key, message or length).
    Ra2(String),
    /// The options do not make sense together.
    Config(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Proto(e) => write!(f, "protocol error: {e}"),
            Self::Closed => write!(f, "connection closed"),
            Self::HandshakeTimeout => write!(f, "handshake timed out"),
            Self::PasswordRequired => write!(f, "the server requires a password"),
            Self::AuthFailed => write!(f, "authentication failed"),
            Self::UsernameRequired => write!(f, "the server requires a username"),
            Self::Untrusted {
                host,
                fingerprint,
                problem,
            } => {
                let what = match problem {
                    CertProblem::Unknown => "is not known".to_owned(),
                    CertProblem::Changed => "has changed since you trusted it".to_owned(),
                    CertProblem::Invalid(why) => format!("is not valid ({why})"),
                };
                write!(
                    f,
                    "the certificate of {host} {what} (SHA-256 {})",
                    format_fingerprint(fingerprint)
                )
            }
            Self::Tls(e) => write!(f, "TLS error: {e}"),
            Self::Ra2(e) => write!(f, "{e}"),
            Self::Config(e) => write!(f, "invalid options: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Proto(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ProtoError> for Error {
    fn from(e: ProtoError) -> Self {
        Self::Proto(e)
    }
}
