//! Apple Remote Desktop authentication (security type 30), the one macOS
//! Screen Sharing offers to Apple's own client and to viewers that sign in
//! with a macOS account (the plain "VNC password" type 2 only reaches the
//! login window).
//!
//! The exchange, as implemented by the other open viewers:
//!
//! 1. The server sends the generator (`U16`), the key length `n` (`U16`), the
//!    prime (`n` bytes) and its public key (`n` bytes).
//! 2. The client picks a random private exponent, computes its public key and
//!    the shared secret by Diffie-Hellman, and derives the AES-128 key as the
//!    MD5 of the shared secret (as `n` big-endian bytes).
//! 3. The client sends 128 bytes, the user name (64 bytes) and the password
//!    (64 bytes), each ended by a NUL and padded with random bytes,
//!    encrypted with AES-128 in ECB mode, then its public key (`n` bytes).
//!
//! This is weak by today's standards and cannot be fixed on our side: the
//! group is not authenticated, so somebody on the path can read the
//! password, and the key derivation is a bare MD5. It is offered only for
//! servers that give nothing better and the caller has to supply the
//! credentials. The exponentiation is not constant time; the private
//! exponent lives for one connection.

use aes::{
    Aes128,
    cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit},
};
use md5::{Digest, Md5};
use num_bigint::BigUint;
use rfb_proto::consts::limits::MAX_DH_KEY_LEN;

/// Bytes of the encrypted credentials block: 64 for the user name, 64 for
/// the password.
pub const CREDENTIALS_LEN: usize = 128;
/// Longest user name or password: 63 bytes and the NUL that ends it.
pub const MAX_CREDENTIAL_LEN: usize = 63;
/// Smallest key length accepted (512 bits). Servers use 1024 bits; anything
/// shorter is not worth talking to.
pub const MIN_KEY_LEN: usize = 64;

/// What the server announced.
#[derive(Debug, Clone, Copy)]
pub struct Params<'a> {
    pub generator: u16,
    pub prime: &'a [u8],
    pub server_key: &'a [u8],
}

/// What the client sends back: the encrypted credentials, then its public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub credentials: [u8; CREDENTIALS_LEN],
    pub public_key: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server's parameters cannot be used.
    BadParams(&'static str),
    /// A user name or password over 63 bytes, or with a NUL inside.
    BadCredential(&'static str),
    /// The system's random source failed.
    Random,
}

impl std::error::Error for Error {}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadParams(why) => write!(f, "unusable Apple DH parameters: {why}"),
            Self::BadCredential(why) => write!(f, "unusable credential: {why}"),
            Self::Random => f.write_str("no random numbers available"),
        }
    }
}

/// Answer the server's parameters with fresh random numbers.
///
/// # Errors
/// [`Error`] for unusable parameters or credentials.
pub fn respond(params: &Params<'_>, username: &[u8], password: &[u8]) -> Result<Reply, Error> {
    let mut private = vec![0u8; params.prime.len().min(MAX_DH_KEY_LEN.into())];
    let mut padding = [0u8; CREDENTIALS_LEN];
    getrandom::fill(&mut private).map_err(|_| Error::Random)?;
    getrandom::fill(&mut padding).map_err(|_| Error::Random)?;
    respond_with(params, username, password, &private, &padding)
}

/// [`respond`] with the random parts given, for tests.
///
/// # Errors
/// As [`respond`].
pub fn respond_with(
    params: &Params<'_>,
    username: &[u8],
    password: &[u8],
    private: &[u8],
    padding: &[u8; CREDENTIALS_LEN],
) -> Result<Reply, Error> {
    let (p, server_key) = check(params)?;
    let width = params.prime.len();
    let g = BigUint::from(params.generator);
    let private = BigUint::from_bytes_be(private);
    if private < BigUint::from(2u8) {
        return Err(Error::Random);
    }
    let public = g.modpow(&private, &p);
    let shared = server_key.modpow(&private, &p);
    let key = aes_key(&shared, width);

    let mut block = *padding;
    put(&mut block[..64], username, "user name")?;
    put(&mut block[64..], password, "password")?;
    let cipher = Aes128::new(&key.into());
    for chunk in block.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(chunk.into());
    }
    Ok(Reply {
        credentials: block,
        public_key: pad_be(&public, width),
    })
}

/// The server's half, for servers and tests: decrypt what a client sent.
/// Returns the user name and the password.
///
/// # Errors
/// [`Error::BadParams`] for unusable parameters or a public key out of range.
pub fn open(
    generator: u16,
    prime: &[u8],
    server_private: &[u8],
    client_key: &[u8],
    credentials: &[u8; CREDENTIALS_LEN],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let params = Params {
        generator,
        prime,
        server_key: client_key,
    };
    let (p, client_key) = check(&params)?;
    let shared = client_key.modpow(&BigUint::from_bytes_be(server_private), &p);
    let key = aes_key(&shared, prime.len());
    let mut block = *credentials;
    let cipher = Aes128::new(&key.into());
    for chunk in block.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(chunk.into());
    }
    let text = |half: &[u8]| {
        half.iter()
            .take_while(|b| **b != 0)
            .copied()
            .collect::<Vec<u8>>()
    };
    Ok((text(&block[..64]), text(&block[64..])))
}

/// The server's public key for a private exponent: `g^private mod prime`.
/// For servers and tests.
///
/// # Errors
/// [`Error::BadParams`] for an unusable prime.
pub fn public_key(generator: u16, prime: &[u8], private: &[u8]) -> Result<Vec<u8>, Error> {
    if prime.len() < MIN_KEY_LEN || prime.len() > usize::from(MAX_DH_KEY_LEN) || prime[0] == 0 {
        return Err(Error::BadParams("key length"));
    }
    let p = BigUint::from_bytes_be(prime);
    let value = BigUint::from(generator).modpow(&BigUint::from_bytes_be(private), &p);
    Ok(pad_be(&value, prime.len()))
}

/// Range checks on what an unauthenticated peer sent: lengths in bounds, an
/// odd prime, a generator of at least 2 and a public key strictly between 1
/// and `p - 1` (1 and `p - 1` would make the shared secret known to the peer).
fn check(params: &Params<'_>) -> Result<(BigUint, BigUint), Error> {
    let width = params.prime.len();
    if !(MIN_KEY_LEN..=usize::from(MAX_DH_KEY_LEN)).contains(&width) {
        return Err(Error::BadParams("key length"));
    }
    if params.server_key.len() != width {
        return Err(Error::BadParams(
            "public key length differs from the prime's",
        ));
    }
    if params.generator < 2 {
        return Err(Error::BadParams("generator"));
    }
    let p = BigUint::from_bytes_be(params.prime);
    // Odd, and really `width` bytes long (a short prime in a long field
    // would hide a small group).
    if !p.bit(0) || p.bits() <= u64::try_from(width * 8 - 8).unwrap_or(0) {
        return Err(Error::BadParams("prime"));
    }
    let key = BigUint::from_bytes_be(params.server_key);
    if key <= BigUint::from(1u8) || key >= &p - 1u8 {
        return Err(Error::BadParams("public key out of range"));
    }
    Ok((p, key))
}

fn put(slot: &mut [u8], text: &[u8], what: &'static str) -> Result<(), Error> {
    if text.len() > MAX_CREDENTIAL_LEN {
        return Err(Error::BadCredential(match what {
            "user name" => "user name over 63 bytes",
            _ => "password over 63 bytes",
        }));
    }
    if text.contains(&0) {
        return Err(Error::BadCredential("contains a NUL"));
    }
    slot[..text.len()].copy_from_slice(text);
    slot[text.len()] = 0;
    Ok(())
}

fn aes_key(shared: &BigUint, width: usize) -> [u8; 16] {
    Md5::digest(pad_be(shared, width)).into()
}

fn pad_be(value: &BigUint, width: usize) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    let mut out = vec![0u8; width.saturating_sub(bytes.len())];
    out.extend_from_slice(&bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 2409 group 2 (1024 bits, generator 2); the numbers below come from
    /// an independent computation in Python (`hashlib.md5`, `cryptography`'s
    /// AES-ECB and `pow`).
    const PRIME: &str = "ffffffffffffffffc90fdaa22168c234c4c6628b80dc1cd129024e088a67cc74020bbea63b139b22514a08798e3404ddef9519b3cd3a431b302b0a6df25f14374fe1356d6d51c245e485b576625e7ec6f44c42e9a637ed6b0bff5cb6f406b7edee386bfb5a899fa5ae9f24117c4b1fe649286651ece65381ffffffffffffffff";
    const SERVER_KEY: &str = "7b1d7d3188d79522e49e1ef8a54a32403fa559965ff46561a8d12fc1f79273e70b2c6ea601992ec15910b87dffa610904bdbc2bdd35b13c95a0d7cb40a0d62720f4a904259982b507d9981f4eedb7371db1336d26e29d58e2d2da00d7928a64cadc4eb683c04df21ac4f5f71174805326a640a789679de2fc1b3ed28d2c9ba71";
    const CLIENT_PUBLIC: &str = "a845c5059be8a1d32bcfa1984114ab431e51e908079492739a6c803eb7d60d8dec2d38f858e0b63ba3550da633a65c29b440c78e795e16cb8b73f0190706e57ed7a721bd24cfb24e7f9888996f930836f88ed992c05c9e12c07a25fcb8be1a6794a6ce483c004dca1e23eb580cfa69fa43fb8e1a09a75f7ad7d3016d68b887bd";
    const CIPHERTEXT: &str = "d171f79df6c570799202aa4f251f2b70011172c8dab1b31f1672f6127626560794a6af1de955abebf2aedc5eeeddc93b380c6ce8f65eb36504dc0f1f597edca029f4bf92b8873e818fc834741085e226e64b2a94197802938e3e26efd9dd7bd5276647c6c36f11dc6a9b56b704969ec643431397cae0ad87b57b1376ab468ae0";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn client_private() -> Vec<u8> {
        (0..128u32)
            .map(|i| u8::try_from((i * 5 + 11) % 256).unwrap())
            .collect()
    }

    fn server_private() -> Vec<u8> {
        (0..128u32)
            .map(|i| u8::try_from((i * 7 + 3) % 256).unwrap())
            .collect()
    }

    fn padding() -> [u8; CREDENTIALS_LEN] {
        core::array::from_fn(|i| u8::try_from((i * 13 + 1) % 256).unwrap())
    }

    #[test]
    fn matches_the_independent_computation() {
        let prime = hex(PRIME);
        let server_key = hex(SERVER_KEY);
        let reply = respond_with(
            &Params {
                generator: 2,
                prime: &prime,
                server_key: &server_key,
            },
            b"jochem",
            b"s3cret pass",
            &client_private(),
            &padding(),
        )
        .unwrap();
        assert_eq!(reply.public_key, hex(CLIENT_PUBLIC));
        assert_eq!(reply.credentials.to_vec(), hex(CIPHERTEXT));
    }

    #[test]
    fn the_server_side_reads_what_the_client_sent() {
        let prime = hex(PRIME);
        let server_key = public_key(2, &prime, &server_private()).unwrap();
        assert_eq!(server_key, hex(SERVER_KEY));
        let reply = respond(
            &Params {
                generator: 2,
                prime: &prime,
                server_key: &server_key,
            },
            "grüße".as_bytes(),
            b"pw",
        )
        .unwrap();
        let (user, password) = open(
            2,
            &prime,
            &server_private(),
            &reply.public_key,
            &reply.credentials,
        )
        .unwrap();
        assert_eq!(user, "grüße".as_bytes());
        assert_eq!(password, b"pw");
    }

    #[test]
    fn unusable_parameters_and_credentials_are_refused() {
        let prime = hex(PRIME);
        let key = hex(SERVER_KEY);
        let ok = Params {
            generator: 2,
            prime: &prime,
            server_key: &key,
        };
        let go = |p: &Params<'_>, u: &[u8], w: &[u8]| respond(p, u, w);
        assert!(go(&ok, b"a", b"b").is_ok());
        assert!(matches!(
            go(&Params { generator: 1, ..ok }, b"a", b"b"),
            Err(Error::BadParams(_))
        ));
        assert!(matches!(
            go(
                &Params {
                    prime: &prime[..32],
                    server_key: &key[..32],
                    ..ok
                },
                b"a",
                b"b"
            ),
            Err(Error::BadParams(_))
        ));
        assert!(matches!(
            go(
                &Params {
                    server_key: &key[1..],
                    ..ok
                },
                b"a",
                b"b"
            ),
            Err(Error::BadParams(_))
        ));
        let one = {
            let mut v = vec![0u8; 128];
            v[127] = 1;
            v
        };
        assert!(matches!(
            go(
                &Params {
                    server_key: &one,
                    ..ok
                },
                b"a",
                b"b"
            ),
            Err(Error::BadParams(_))
        ));
        let mut minus_one = prime.clone();
        *minus_one.last_mut().unwrap() -= 1;
        assert!(matches!(
            go(
                &Params {
                    server_key: &minus_one,
                    ..ok
                },
                b"a",
                b"b"
            ),
            Err(Error::BadParams(_))
        ));
        let even = {
            let mut v = prime.clone();
            *v.last_mut().unwrap() &= !1;
            v
        };
        assert!(matches!(
            go(&Params { prime: &even, ..ok }, b"a", b"b"),
            Err(Error::BadParams(_))
        ));
        assert!(matches!(
            go(&ok, &[b'x'; 64], b"b"),
            Err(Error::BadCredential(_))
        ));
        assert!(go(&ok, &[b'x'; 63], b"b").is_ok());
        assert!(matches!(
            go(&ok, b"a\0b", b"b"),
            Err(Error::BadCredential(_))
        ));
    }
}
