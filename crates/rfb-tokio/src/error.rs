use core::fmt;

use rfb_proto::ProtoError;

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
