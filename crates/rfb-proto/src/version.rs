//! The 12-byte `ProtocolVersion` message: `"RFB xxx.yyy\n"`.

use crate::ProtoError;

/// RFB protocol versions we speak. 3.3 is supported in both roles (decision
/// from 2026-10-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProtocolVersion {
    V3_3,
    V3_7,
    V3_8,
}

impl ProtocolVersion {
    /// Length of the message on the wire.
    pub const LEN: usize = 12;

    /// The exact bytes sent on the wire.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; Self::LEN] {
        match self {
            Self::V3_3 => *b"RFB 003.003\n",
            Self::V3_7 => *b"RFB 003.007\n",
            Self::V3_8 => *b"RFB 003.008\n",
        }
    }

    /// Parse the raw `major.minor` numbers from a 12-byte message.
    ///
    /// # Errors
    /// [`ProtoError::NeedMore`] if `buf` is shorter than 12 bytes,
    /// [`ProtoError::Malformed`] if the text is not `RFB ddd.ddd\n`.
    pub fn parse_raw(buf: &[u8]) -> Result<(u16, u16), ProtoError> {
        if buf.len() < Self::LEN {
            return Err(ProtoError::NeedMore(Self::LEN - buf.len()));
        }
        let b = &buf[..Self::LEN];
        if &b[..4] != b"RFB " || b[7] != b'.' || b[11] != b'\n' {
            return Err(ProtoError::Malformed {
                ctx: "ProtocolVersion",
            });
        }
        let num = |s: &[u8]| -> Result<u16, ProtoError> {
            s.iter().try_fold(0u16, |acc, &c| {
                if c.is_ascii_digit() {
                    Ok(acc * 10 + u16::from(c - b'0'))
                } else {
                    Err(ProtoError::Malformed {
                        ctx: "ProtocolVersion digits",
                    })
                }
            })
        };
        Ok((num(&b[4..7])?, num(&b[8..11])?))
    }

    /// Map a version announced by a peer to the version we will use.
    ///
    /// Rules from the community spec and real-world servers:
    /// - 3.3, 3.7 and 3.8 map to themselves.
    /// - 3.5 (sent by some old clients) and other 3.x below 3.7 mean 3.3.
    /// - 3.x above 3.8 (Apple sends 3.889) means 3.8.
    ///
    /// # Errors
    /// [`ProtoError::UnsupportedVersion`] for any major version other than 3.
    pub fn from_announced(major: u16, minor: u16) -> Result<Self, ProtoError> {
        match (major, minor) {
            (3, 8..) => Ok(Self::V3_8),
            (3, 7) => Ok(Self::V3_7),
            (3, _) => Ok(Self::V3_3),
            _ => Err(ProtoError::UnsupportedVersion { major, minor }),
        }
    }

    /// Client side: the version to answer with, given the server's offer and
    /// the highest version we allow.
    #[must_use]
    pub fn client_choice(server_offer: Self, our_max: Self) -> Self {
        server_offer.min(our_max)
    }
}

#[cfg(test)]
mod tests {
    use super::ProtocolVersion as V;
    use crate::ProtoError;

    #[test]
    fn round_trips_all_versions() {
        for v in [V::V3_3, V::V3_7, V::V3_8] {
            let (maj, min) = V::parse_raw(&v.to_bytes()).unwrap();
            assert_eq!(V::from_announced(maj, min), Ok(v));
        }
    }

    #[test]
    fn maps_odd_versions() {
        assert_eq!(V::from_announced(3, 5), Ok(V::V3_3));
        assert_eq!(V::from_announced(3, 889), Ok(V::V3_8));
        assert!(V::from_announced(4, 0).is_err());
    }

    #[test]
    fn rejects_garbage_and_reports_short_input() {
        assert_eq!(V::parse_raw(b"RFB 003"), Err(ProtoError::NeedMore(5)));
        assert!(V::parse_raw(b"HTTP/1.1 200").is_err());
        assert!(V::parse_raw(b"RFB 003.00x\n").is_err());
    }

    #[test]
    fn client_never_exceeds_server_offer() {
        assert_eq!(V::client_choice(V::V3_7, V::V3_8), V::V3_7);
        assert_eq!(V::client_choice(V::V3_8, V::V3_8), V::V3_8);
    }
}
