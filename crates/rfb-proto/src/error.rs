use alloc::string::String;
use core::fmt;

/// Errors produced by the protocol core.
///
/// Every variant except [`ProtoError::NeedMore`] is fatal: RFB has no point at
/// which a stream could be re-synchronised, so the connection must be closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    /// More input is needed; the payload is the minimum number of extra bytes.
    NeedMore(usize),
    /// The input violates the protocol; `ctx` names the field.
    Malformed { ctx: &'static str },
    /// A length field exceeds the configured limit.
    LimitExceeded {
        field: &'static str,
        value: u64,
        limit: u64,
    },
    /// The peer announced a protocol version we do not speak.
    UnsupportedVersion { major: u16, minor: u16 },
    /// The peer rejected the security handshake; the reason is lossy UTF-8.
    SecurityFailed(Option<String>),
    /// None of the peer's security types is one we allow.
    NoAcceptableSecurity,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedMore(n) => write!(f, "need {n} more byte(s)"),
            Self::Malformed { ctx } => write!(f, "malformed {ctx}"),
            Self::LimitExceeded {
                field,
                value,
                limit,
            } => {
                write!(f, "{field} is {value}, limit is {limit}")
            }
            Self::UnsupportedVersion { major, minor } => {
                write!(f, "unsupported RFB version {major}.{minor}")
            }
            Self::SecurityFailed(Some(r)) => write!(f, "security failed: {r}"),
            Self::SecurityFailed(None) => write!(f, "security failed"),
            Self::NoAcceptableSecurity => write!(f, "no acceptable security type"),
        }
    }
}

impl core::error::Error for ProtoError {}
