use core::fmt;

/// Why a rectangle could not be decoded. The stream itself is still in sync
/// (framing already succeeded), but the picture would be wrong, so a viewer
/// should treat every error as fatal for the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The payload does not match what the encoding and rectangle imply.
    Malformed(&'static str),
    /// The rectangle (or a `CopyRect` source) lies outside the framebuffer.
    OutOfBounds,
    /// A pixel format we cannot convert (for example colour-mapped).
    UnsupportedFormat(&'static str),
    /// An encoding we cannot decode yet.
    UnsupportedEncoding(i32),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed payload: {what}"),
            Self::OutOfBounds => write!(f, "rectangle outside the framebuffer"),
            Self::UnsupportedFormat(what) => write!(f, "unsupported pixel format: {what}"),
            Self::UnsupportedEncoding(e) => write!(f, "unsupported encoding {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}
