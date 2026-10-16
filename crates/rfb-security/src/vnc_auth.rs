//! VNC Authentication (security type 2): the server sends a 16-byte random
//! challenge, the client returns it DES-ECB encrypted with the password as
//! key (RFB community spec, "VNC Authentication").
//!
//! Weak by design (8-character password, DES, no encryption of the session).
//! Kept for compatibility only.

use des::{
    Des,
    cipher::{Block, BlockCipherEncrypt, KeyInit},
};

/// Compute the 16-byte response to `challenge`.
///
/// The key is the password truncated or zero-padded to 8 bytes, with the bits
/// of every byte reversed (a quirk of the original implementation that the
/// spec documents).
#[must_use]
pub fn response(password: &[u8], challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, p) in key.iter_mut().zip(password) {
        *k = p.reverse_bits();
    }
    let cipher = Des::new(&key.into());
    let mut out = [0u8; 16];
    for offset in [0, 8] {
        let mut block =
            Block::<Des>::from(core::array::from_fn::<u8, 8, _>(|i| challenge[offset + i]));
        cipher.encrypt_block(&mut block);
        out[offset..offset + 8].copy_from_slice(&block);
    }
    out
}

/// Check a client's `response` against the `challenge` we sent. The compare
/// does not stop at the first differing byte.
#[must_use]
pub fn verify(password: &[u8], challenge: &[u8; 16], response_: &[u8; 16]) -> bool {
    let expected = response(password, challenge);
    expected
        .iter()
        .zip(response_)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use core::fmt::Write;

    use super::*;

    const CHALLENGE: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    /// Vectors produced with an independent implementation:
    /// `openssl enc -des-ecb -nopad -K <bit-reversed, zero-padded key>`.
    #[test]
    fn matches_openssl_vectors() {
        let cases: [(&[u8], &str); 3] = [
            (b"password", "b866924125c8eebb9debc1db61c538e2"),
            (b"abc", "9c22b4f2088c3465a1562c4b9d6edb04"),
            // Only the first 8 bytes of the password count.
            (b"longerthan8chars", "76543820b2c358fe359083c192fa77c9"),
        ];
        for (pw, want) in cases {
            let got = response(pw, &CHALLENGE);
            let mut hex = String::new();
            for b in got {
                write!(hex, "{b:02x}").unwrap();
            }
            assert_eq!(hex, want, "password {pw:?}");
        }
    }

    #[test]
    fn verify_accepts_right_and_rejects_wrong() {
        let good = response(b"secret", &CHALLENGE);
        assert!(verify(b"secret", &CHALLENGE, &good));
        assert!(!verify(b"Secret", &CHALLENGE, &good));
        let mut bad = good;
        bad[15] ^= 1;
        assert!(!verify(b"secret", &CHALLENGE, &bad));
    }
}
