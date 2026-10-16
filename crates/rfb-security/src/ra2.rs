//! RSA-AES security types (`RA2`, `RA2ne`, `RA2_256`, `RA2ne_256`): the RSA key
//! exchange, the key derivation and the AES-EAX message layer.
//!
//! The functions here are pure (no I/O). `rfb-tokio` runs the handshake
//! messages over a stream with them. See rfbproto.rst, "RSA-AES Security
//! Type".

use aes::{Aes128, Aes256};
use eax::{
    Eax,
    aead::{Aead as _, KeyInit, Payload},
};
use rand_core::UnwrapErr;
use rfb_proto::consts::security;
use rsa::{BoxedUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey, traits::PublicKeyParts};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// Smallest and largest RSA modulus accepted from the peer, in bits.
pub const MIN_KEY_BITS: u32 = 1024;
pub const MAX_KEY_BITS: u32 = 8192;
/// Modulus size of the keys we generate.
pub const KEY_BITS: usize = 2048;
/// Longest plaintext of one encrypted message the peer may send us.
pub const MAX_MESSAGE: usize = 8192;
/// Size of the random numbers the two sides exchange.
pub const RANDOM_LEN: usize = 16;
/// Size of the EAX authentication tag.
pub const TAG_LEN: usize = 16;

/// Why an RSA-AES step failed. All are fatal for the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ra2Error {
    /// The peer's public key has an unacceptable size or form.
    BadKey(&'static str),
    /// RSA encryption or decryption failed (a corrupt or wrong-key message).
    Rsa,
    /// A message failed authentication: damaged, replayed or forged.
    Authentication,
    /// A length field is out of range.
    BadLength(&'static str),
    /// Random numbers were unavailable.
    Random,
}

impl std::fmt::Display for Ra2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadKey(why) => write!(f, "RSA-AES: bad public key: {why}"),
            Self::Rsa => f.write_str("RSA-AES: RSA operation failed"),
            Self::Authentication => f.write_str("RSA-AES: message authentication failed"),
            Self::BadLength(what) => write!(f, "RSA-AES: bad length: {what}"),
            Self::Random => f.write_str("RSA-AES: no random numbers available"),
        }
    }
}

impl std::error::Error for Ra2Error {}

/// The security types of this family and what differs between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// `RA2` and `RA2ne`: SHA-1 and AES-128.
    Aes128,
    /// `RA2_256` and `RA2ne_256`: SHA-256 and AES-256.
    Aes256,
}

impl Variant {
    /// The variant for a security type, and whether the traffic after the
    /// handshake stays encrypted (`false` for the `ne` types, where only the
    /// handshake is encrypted). `None` for any other type, including the
    /// two-step types, which are not supported.
    #[must_use]
    pub fn from_security(security_type: u8) -> Option<(Self, bool)> {
        match security_type {
            security::RA2 => Some((Self::Aes128, true)),
            security::RA2NE => Some((Self::Aes128, false)),
            security::RA2_256 => Some((Self::Aes256, true)),
            security::RA2NE_256 => Some((Self::Aes256, false)),
            _ => None,
        }
    }

    /// The hash of the concatenated `parts`: SHA-1 (20 bytes) or SHA-256.
    fn hash(self, parts: &[&[u8]]) -> Vec<u8> {
        match self {
            Self::Aes128 => {
                let mut h = Sha1::new();
                for p in parts {
                    h.update(p);
                }
                h.finalize().to_vec()
            }
            Self::Aes256 => {
                let mut h = Sha256::new();
                for p in parts {
                    h.update(p);
                }
                h.finalize().to_vec()
            }
        }
    }

    /// Session key length: the first 16 bytes of the SHA-1, or all of the
    /// SHA-256.
    fn key_len(self) -> usize {
        match self {
            Self::Aes128 => 16,
            Self::Aes256 => 32,
        }
    }
}

/// An RSA public key as it is sent: a bit length, then the modulus and the
/// public exponent, each `ceil(bits / 8)` bytes, big endian.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    bits: u32,
    modulus: Vec<u8>,
    exponent: Vec<u8>,
}

impl PublicKey {
    /// From the received parts. `modulus` and `exponent` must each be
    /// `ceil(bits / 8)` bytes.
    ///
    /// # Errors
    /// [`Ra2Error::BadKey`] if the size is outside [`MIN_KEY_BITS`] to
    /// [`MAX_KEY_BITS`] or the parts do not fit it.
    pub fn from_parts(bits: u32, modulus: &[u8], exponent: &[u8]) -> Result<Self, Ra2Error> {
        if !(MIN_KEY_BITS..=MAX_KEY_BITS).contains(&bits) {
            return Err(Ra2Error::BadKey("size out of range"));
        }
        let len = Self::part_len(bits);
        if modulus.len() != len || exponent.len() != len {
            return Err(Ra2Error::BadKey("modulus or exponent has the wrong length"));
        }
        let key = Self {
            bits,
            modulus: modulus.to_vec(),
            exponent: exponent.to_vec(),
        };
        // Reject what RSA itself would reject (even modulus, tiny exponent).
        key.rsa()?;
        Ok(key)
    }

    /// Bytes of the modulus and of the exponent for a key of `bits` bits.
    #[must_use]
    pub fn part_len(bits: u32) -> usize {
        bits.div_ceil(8) as usize
    }

    #[must_use]
    pub fn bits(&self) -> u32 {
        self.bits
    }

    /// The wire form: used as is in the exchange hashes and for the
    /// fingerprint.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 2 * self.modulus.len());
        out.extend_from_slice(&self.bits.to_be_bytes());
        out.extend_from_slice(&self.modulus);
        out.extend_from_slice(&self.exponent);
        out
    }

    /// SHA-256 of [`PublicKey::encode`], what a user compares or pins.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(self.encode()).into()
    }

    fn rsa(&self) -> Result<RsaPublicKey, Ra2Error> {
        let bits_precision = u32::try_from(self.modulus.len() * 8).map_err(|_| Ra2Error::Rsa)?;
        let n = BoxedUint::from_be_slice(&self.modulus, bits_precision)
            .map_err(|_| Ra2Error::BadKey("modulus"))?;
        let e = BoxedUint::from_be_slice(&self.exponent, bits_precision)
            .map_err(|_| Ra2Error::BadKey("exponent"))?;
        RsaPublicKey::new(n, e).map_err(|_| Ra2Error::BadKey("not a valid RSA key"))
    }

    /// Encrypt `message` with EME-PKCS1-v1_5; the result is
    /// `ceil(bits / 8)` bytes.
    ///
    /// # Errors
    /// [`Ra2Error::Rsa`] on failure.
    pub fn encrypt(&self, message: &[u8]) -> Result<Vec<u8>, Ra2Error> {
        self.rsa()?
            .encrypt(&mut rng(), Pkcs1v15Encrypt, message)
            .map_err(|_| Ra2Error::Rsa)
    }
}

fn rng() -> UnwrapErr<getrandom::SysRng> {
    UnwrapErr(getrandom::SysRng)
}

/// Our own RSA key pair.
#[derive(Debug, Clone)]
pub struct KeyPair {
    private: RsaPrivateKey,
    public: PublicKey,
}

impl KeyPair {
    /// Generate a pair of [`KEY_BITS`] bits (takes a moment).
    ///
    /// # Errors
    /// [`Ra2Error::Random`] if no randomness is available.
    pub fn generate() -> Result<Self, Ra2Error> {
        let private = RsaPrivateKey::new(&mut rng(), KEY_BITS).map_err(|_| Ra2Error::Random)?;
        Self::from_private(private)
    }

    /// Wrap a private key (for example one loaded from a file).
    ///
    /// # Errors
    /// [`Ra2Error::BadKey`] if its size is out of range.
    pub fn from_private(private: RsaPrivateKey) -> Result<Self, Ra2Error> {
        let bits = private.n().bits();
        let len = PublicKey::part_len(bits);
        let pad = |bytes: Box<[u8]>| {
            let mut v = vec![0u8; len.saturating_sub(bytes.len())];
            v.extend_from_slice(&bytes);
            v
        };
        let public = PublicKey::from_parts(bits, &pad(private.n_bytes()), &pad(private.e_bytes()))?;
        Ok(Self { private, public })
    }

    /// The private key, to store it.
    #[must_use]
    pub fn private(&self) -> &RsaPrivateKey {
        &self.private
    }

    #[must_use]
    pub fn public(&self) -> &PublicKey {
        &self.public
    }

    /// Decrypt a message that was encrypted to our public key.
    ///
    /// # Errors
    /// [`Ra2Error::Rsa`] if it does not decrypt.
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, Ra2Error> {
        self.private
            .decrypt(Pkcs1v15Encrypt, ciphertext)
            .map_err(|_| Ra2Error::Rsa)
    }
}

/// A fresh random number for the exchange.
///
/// # Errors
/// [`Ra2Error::Random`] if no randomness is available.
pub fn random() -> Result<[u8; RANDOM_LEN], Ra2Error> {
    let mut out = [0u8; RANDOM_LEN];
    getrandom::fill(&mut out).map_err(|_| Ra2Error::Random)?;
    Ok(out)
}

/// The two session keys, `(client, server)`: the client key protects what
/// the client sends.
#[must_use]
pub fn derive_keys(
    variant: Variant,
    server_random: &[u8; RANDOM_LEN],
    client_random: &[u8; RANDOM_LEN],
) -> (Vec<u8>, Vec<u8>) {
    let n = variant.key_len();
    let client = variant.hash(&[server_random, client_random])[..n].to_vec();
    let server = variant.hash(&[client_random, server_random])[..n].to_vec();
    (client, server)
}

/// The hash the server proves itself with: over its key then the client's.
/// The client's hash is the same function with the arguments swapped.
#[must_use]
pub fn exchange_hash(variant: Variant, first: &PublicKey, second: &PublicKey) -> Vec<u8> {
    variant.hash(&[&first.encode(), &second.encode()])
}

enum Aead {
    Aes128(Box<Eax<Aes128>>),
    Aes256(Box<Eax<Aes256>>),
}

impl Aead {
    fn new(variant: Variant, key: &[u8]) -> Self {
        // The keys come from `derive_keys`, so their length is right.
        match variant {
            Variant::Aes128 => Self::Aes128(Box::new(
                Eax::<Aes128>::new_from_slice(key).expect("16 byte key"),
            )),
            Variant::Aes256 => Self::Aes256(Box::new(
                Eax::<Aes256>::new_from_slice(key).expect("32 byte key"),
            )),
        }
    }

    fn encrypt(&self, nonce: &[u8; 16], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, Ra2Error> {
        let payload = Payload { msg: data, aad };
        match self {
            Self::Aes128(c) => c.encrypt(nonce.into(), payload),
            Self::Aes256(c) => c.encrypt(nonce.into(), payload),
        }
        .map_err(|_| Ra2Error::Authentication)
    }

    fn decrypt(&self, nonce: &[u8; 16], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, Ra2Error> {
        let payload = Payload { msg: data, aad };
        match self {
            Self::Aes128(c) => c.decrypt(nonce.into(), payload),
            Self::Aes256(c) => c.decrypt(nonce.into(), payload),
        }
        .map_err(|_| Ra2Error::Authentication)
    }
}

/// The message layer: each message is a 2-byte length (also the associated
/// data), the AES-EAX ciphertext and a 16-byte tag. The nonce is a 16-byte
/// little-endian message counter, per direction, starting at zero.
pub struct Cipher {
    send: Aead,
    receive: Aead,
    send_counter: u128,
    receive_counter: u128,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cipher")
            .field("sent", &self.send_counter)
            .field("received", &self.receive_counter)
            .finish_non_exhaustive()
    }
}

impl Cipher {
    /// A cipher that encrypts with `send_key` and decrypts with
    /// `receive_key` (both from [`derive_keys`], swapped between the sides).
    #[must_use]
    pub fn new(variant: Variant, send_key: &[u8], receive_key: &[u8]) -> Self {
        Self {
            send: Aead::new(variant, send_key),
            receive: Aead::new(variant, receive_key),
            send_counter: 0,
            receive_counter: 0,
        }
    }

    /// Encrypt one message (at most 65535 bytes): the bytes to send,
    /// header and tag included.
    ///
    /// # Errors
    /// [`Ra2Error::BadLength`] if `plaintext` is too long.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, Ra2Error> {
        let len = u16::try_from(plaintext.len()).map_err(|_| Ra2Error::BadLength("message"))?;
        let header = len.to_be_bytes();
        let nonce = self.send_counter.to_le_bytes();
        self.send_counter += 1;
        let body = self.send.encrypt(&nonce, &header, plaintext)?;
        let mut out = Vec::with_capacity(2 + body.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decrypt one received message. `header` is its two length bytes and
    /// `body` the ciphertext followed by the tag (`length + 16` bytes).
    ///
    /// # Errors
    /// [`Ra2Error::Authentication`] if it was damaged, replayed or forged.
    pub fn open(&mut self, header: [u8; 2], body: &[u8]) -> Result<Vec<u8>, Ra2Error> {
        let nonce = self.receive_counter.to_le_bytes();
        let plain = self.receive.decrypt(&nonce, &header, body)?;
        self.receive_counter += 1;
        Ok(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsa_keys_round_trip_through_their_wire_form() {
        let pair = KeyPair::generate().unwrap();
        let wire = pair.public().encode();
        assert_eq!(wire.len(), 4 + 2 * 256);
        assert_eq!(&wire[..4], 2048u32.to_be_bytes());
        let (bits, rest) = (
            u32::from_be_bytes(wire[..4].try_into().unwrap()),
            &wire[4..],
        );
        let parsed = PublicKey::from_parts(bits, &rest[..256], &rest[256..]).unwrap();
        assert_eq!(&parsed, pair.public());
        // Encrypt to the parsed key, decrypt with the pair.
        let secret = random().unwrap();
        let ct = parsed.encrypt(&secret).unwrap();
        assert_eq!(ct.len(), 256);
        assert_eq!(pair.decrypt(&ct).unwrap(), secret);
        // A damaged ciphertext does not decrypt to the secret.
        let mut bad = ct;
        bad[10] ^= 1;
        assert_ne!(pair.decrypt(&bad).ok().as_deref(), Some(&secret[..]));
    }

    #[test]
    fn keys_of_the_wrong_size_or_form_are_refused() {
        let ok = vec![0xFF; 128];
        assert!(matches!(
            PublicKey::from_parts(512, &ok[..64], &ok[..64]),
            Err(Ra2Error::BadKey(_))
        ));
        assert!(matches!(
            PublicKey::from_parts(9000, &[1; 1125], &[1; 1125]),
            Err(Ra2Error::BadKey(_))
        ));
        assert!(matches!(
            PublicKey::from_parts(1024, &ok, &ok[..127]),
            Err(Ra2Error::BadKey(_))
        ));
        // An even modulus is not an RSA modulus.
        let mut even = vec![0xFF; 128];
        even[127] = 0xFE;
        let mut e = vec![0; 128];
        e[127] = 3;
        assert!(matches!(
            PublicKey::from_parts(1024, &even, &e),
            Err(Ra2Error::BadKey(_))
        ));
    }

    #[test]
    fn session_keys_follow_the_spec_formulas() {
        let (sr, cr) = ([1u8; 16], [2u8; 16]);
        let (client, server) = derive_keys(Variant::Aes128, &sr, &cr);
        // First 16 bytes of SHA1(ServerRandom || ClientRandom) and the other order.
        let sha1 = |a: &[u8; 16], b: &[u8; 16]| {
            let mut h = Sha1::new();
            h.update(a);
            h.update(b);
            h.finalize()[..16].to_vec()
        };
        assert_eq!(client, sha1(&sr, &cr));
        assert_eq!(server, sha1(&cr, &sr));
        let (client, server) = derive_keys(Variant::Aes256, &sr, &cr);
        assert_eq!(client, Sha256::digest([&sr[..], &cr[..]].concat()).to_vec());
        assert_eq!(server, Sha256::digest([&cr[..], &sr[..]].concat()).to_vec());
    }

    #[test]
    fn messages_round_trip_and_each_direction_counts_on_its_own() {
        for variant in [Variant::Aes128, Variant::Aes256] {
            let (ck, sk) = derive_keys(variant, &[3; 16], &[4; 16]);
            let mut client = Cipher::new(variant, &ck, &sk);
            let mut server = Cipher::new(variant, &sk, &ck);
            for i in 0..5u8 {
                let msg = vec![i; usize::from(i) * 7];
                let sealed = client.seal(&msg).unwrap();
                assert_eq!(sealed.len(), 2 + msg.len() + TAG_LEN);
                assert_eq!(
                    &sealed[..2],
                    u16::try_from(msg.len()).unwrap().to_be_bytes()
                );
                let opened = server.open([sealed[0], sealed[1]], &sealed[2..]).unwrap();
                assert_eq!(opened, msg);
                let reply = server.seal(b"ok").unwrap();
                assert_eq!(
                    client.open([reply[0], reply[1]], &reply[2..]).unwrap(),
                    b"ok"
                );
            }
        }
    }

    #[test]
    fn damaged_replayed_reordered_and_misdirected_messages_fail() {
        let variant = Variant::Aes256;
        let (ck, sk) = derive_keys(variant, &[3; 16], &[4; 16]);
        let mut client = Cipher::new(variant, &ck, &sk);
        let mut server = Cipher::new(variant, &sk, &ck);
        let first = client.seal(b"first").unwrap();
        let second = client.seal(b"second").unwrap();
        let split = |m: &[u8]| ([m[0], m[1]], m[2..].to_vec());
        // Out of order: the nonce counter does not match.
        let (h2, b2) = split(&second);
        assert_eq!(server.open(h2, &b2), Err(Ra2Error::Authentication));
        // The right order works, and a failure did not advance the counter.
        let (h1, b1) = split(&first);
        assert_eq!(server.open(h1, &b1).unwrap(), b"first");
        assert_eq!(server.open(h2, &b2).unwrap(), b"second");
        // Replay.
        assert_eq!(server.open(h1, &b1), Err(Ra2Error::Authentication));
        // A flipped ciphertext bit, tag bit, or length header.
        let third = client.seal(b"third").unwrap();
        let (h3, b3) = split(&third);
        let mut bad = b3.clone();
        bad[0] ^= 1;
        assert_eq!(server.open(h3, &bad), Err(Ra2Error::Authentication));
        let mut bad = b3.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(server.open(h3, &bad), Err(Ra2Error::Authentication));
        assert_eq!(
            server.open([h3[0], h3[1] ^ 1], &b3),
            Err(Ra2Error::Authentication)
        );
        assert_eq!(server.open(h3, &b3).unwrap(), b"third");
        // A message turned around: the client's own key does not open it.
        let mut echo = Cipher::new(variant, &ck, &sk);
        let mine = echo.seal(b"x").unwrap();
        assert_eq!(
            echo.open([mine[0], mine[1]], &mine[2..]),
            Err(Ra2Error::Authentication)
        );
        assert!(matches!(
            Cipher::new(variant, &ck, &sk).seal(&vec![0; 70_000]),
            Err(Ra2Error::BadLength(_))
        ));
    }

    #[test]
    fn exchange_hashes_cover_both_keys_in_order() {
        let a = KeyPair::generate().unwrap();
        let b = KeyPair::generate().unwrap();
        let h = exchange_hash(Variant::Aes128, a.public(), b.public());
        assert_eq!(h.len(), 20);
        assert_ne!(h, exchange_hash(Variant::Aes128, b.public(), a.public()));
        assert_eq!(
            exchange_hash(Variant::Aes256, a.public(), b.public()).len(),
            32
        );
    }

    #[test]
    fn the_security_types_map_to_variants() {
        assert_eq!(Variant::from_security(5), Some((Variant::Aes128, true)));
        assert_eq!(Variant::from_security(6), Some((Variant::Aes128, false)));
        assert_eq!(Variant::from_security(129), Some((Variant::Aes256, true)));
        assert_eq!(Variant::from_security(130), Some((Variant::Aes256, false)));
        assert_eq!(
            Variant::from_security(13),
            None,
            "two-step is not supported"
        );
        assert_eq!(Variant::from_security(133), None);
        assert_eq!(Variant::from_security(2), None);
    }
}
