//! The RSA-AES handshake over a stream, and the stream that carries the
//! encrypted traffic afterwards (rfbproto.rst, "RSA-AES Security Type").
//! The cryptography is in `rfb_security::ra2`; this is the I/O around it.

use std::{
    io,
    pin::Pin,
    sync::OnceLock,
    task::{Context, Poll, ready},
};

use rfb_security::{
    ra2::{self, Cipher, KeyPair, MAX_MESSAGE, PublicKey, RANDOM_LEN, Ra2Error, TAG_LEN, Variant},
    tls::{CertProblem, Fingerprint},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::Error;

/// How the client decides to trust the server's RSA key.
#[derive(Debug, Clone)]
pub struct Ra2Options {
    /// The host the user typed, for the trust prompt.
    pub server_name: String,
    /// SHA-256 fingerprints ([`PublicKey::fingerprint`]) the user trusts.
    pub pins: Vec<Fingerprint>,
}

fn fail(e: &Ra2Error) -> Error {
    Error::Ra2(e.to_string())
}

async fn read_vec<S: AsyncRead + Unpin>(stream: &mut S, len: usize) -> Result<Vec<u8>, Error> {
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::Closed
        } else {
            Error::Io(e)
        }
    })?;
    Ok(buf)
}

async fn read_public_key<S: AsyncRead + Unpin>(stream: &mut S) -> Result<PublicKey, Error> {
    let head = read_vec(stream, 4).await?;
    let bits = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    // Check the size before reading what it announces.
    if !(ra2::MIN_KEY_BITS..=ra2::MAX_KEY_BITS).contains(&bits) {
        return Err(fail(&Ra2Error::BadKey("size out of range")));
    }
    let len = PublicKey::part_len(bits);
    let parts = read_vec(stream, 2 * len).await?;
    PublicKey::from_parts(bits, &parts[..len], &parts[len..]).map_err(|e| fail(&e))
}

/// A `U16` length and that many bytes: an RSA-encrypted random number.
async fn read_encrypted_random<S: AsyncRead + Unpin>(
    stream: &mut S,
    key: &KeyPair,
) -> Result<[u8; RANDOM_LEN], Error> {
    let head = read_vec(stream, 2).await?;
    let len = usize::from(u16::from_be_bytes([head[0], head[1]]));
    if len != PublicKey::part_len(key.public().bits()) {
        return Err(fail(&Ra2Error::BadLength("encrypted random number")));
    }
    let ct = read_vec(stream, len).await?;
    let plain = key.decrypt(&ct).map_err(|e| fail(&e))?;
    <[u8; RANDOM_LEN]>::try_from(plain.as_slice())
        .map_err(|_| fail(&Ra2Error::BadLength("random number")))
}

async fn write_encrypted_random<S: AsyncWrite + Unpin>(
    stream: &mut S,
    peer: &PublicKey,
    random: &[u8; RANDOM_LEN],
) -> Result<(), Error> {
    let ct = peer.encrypt(random).map_err(|e| fail(&e))?;
    let mut out = Vec::with_capacity(2 + ct.len());
    out.extend_from_slice(
        &u16::try_from(ct.len())
            .map_err(|_| Error::Config("key"))?
            .to_be_bytes(),
    );
    out.extend_from_slice(&ct);
    stream.write_all(&out).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_message<S: AsyncRead + Unpin>(
    stream: &mut S,
    cipher: &mut Cipher,
) -> Result<Vec<u8>, Error> {
    let head = read_vec(stream, 2).await?;
    let len = usize::from(u16::from_be_bytes([head[0], head[1]]));
    if len > MAX_MESSAGE {
        return Err(fail(&Ra2Error::BadLength("message")));
    }
    let body = read_vec(stream, len + TAG_LEN).await?;
    cipher.open([head[0], head[1]], &body).map_err(|e| fail(&e))
}

async fn write_message<S: AsyncWrite + Unpin>(
    stream: &mut S,
    cipher: &mut Cipher,
    plaintext: &[u8],
) -> Result<(), Error> {
    stream
        .write_all(&cipher.seal(plaintext).map_err(|e| fail(&e))?)
        .await?;
    stream.flush().await?;
    Ok(())
}

/// Equality that does not stop at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Our key for the client role, made once per process: generating one takes
/// a moment and it carries no identity.
async fn client_key() -> Result<&'static KeyPair, Error> {
    static KEY: OnceLock<KeyPair> = OnceLock::new();
    if let Some(k) = KEY.get() {
        return Ok(k);
    }
    let generated = tokio::task::spawn_blocking(KeyPair::generate)
        .await
        .map_err(|e| Error::Io(io::Error::other(e)))?
        .map_err(|e| fail(&e))?;
    Ok(KEY.get_or_init(|| generated))
}

/// What a finished handshake leaves behind.
pub(crate) struct Established {
    pub cipher: Cipher,
    /// The user name, on the server side.
    pub username: Option<Vec<u8>>,
}

/// Run the client side up to and including sending the credentials.
pub(crate) async fn client_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    variant: Variant,
    opts: &Ra2Options,
    username: Option<&[u8]>,
    password: &[u8],
) -> Result<Established, Error> {
    let server_pub = read_public_key(stream).await?;
    let fingerprint = server_pub.fingerprint();
    // Trust the key only if the user pinned it: it is what proves who the
    // server is, so without a pin the caller must ask.
    if !opts.pins.contains(&fingerprint) {
        return Err(Error::Untrusted {
            host: opts.server_name.clone(),
            fingerprint,
            problem: if opts.pins.is_empty() {
                CertProblem::Unknown
            } else {
                CertProblem::Changed
            },
        });
    }
    let pair = client_key().await?;
    stream.write_all(&pair.public().encode()).await?;
    stream.flush().await?;
    let server_random = read_encrypted_random(stream, pair).await?;
    let client_random = ra2::random().map_err(|e| fail(&e))?;
    write_encrypted_random(stream, &server_pub, &client_random).await?;
    let (client_key, server_key) = ra2::derive_keys(variant, &server_random, &client_random);
    let mut cipher = Cipher::new(variant, &client_key, &server_key);

    let server_hash = read_message(stream, &mut cipher).await?;
    if !same(
        &server_hash,
        &ra2::exchange_hash(variant, &server_pub, pair.public()),
    ) {
        return Err(fail(&Ra2Error::Authentication));
    }
    let ours = ra2::exchange_hash(variant, pair.public(), &server_pub);
    write_message(stream, &mut cipher, &ours).await?;

    let subtype = read_message(stream, &mut cipher).await?;
    let wants_username = match subtype.as_slice() {
        [1] => true,
        [2] => false,
        _ => return Err(fail(&Ra2Error::BadLength("subtype"))),
    };
    let user = if wants_username {
        username.ok_or(Error::UsernameRequired)?
    } else {
        &[]
    };
    let credentials = credentials(user, password)?;
    write_message(stream, &mut cipher, &credentials).await?;
    Ok(Established {
        cipher,
        username: None,
    })
}

fn credentials(username: &[u8], password: &[u8]) -> Result<Vec<u8>, Error> {
    let (ulen, plen) = (
        u8::try_from(username.len())
            .map_err(|_| Error::Config("username longer than 255 bytes"))?,
        u8::try_from(password.len())
            .map_err(|_| Error::Config("password longer than 255 bytes"))?,
    );
    let mut out = Vec::with_capacity(2 + username.len() + password.len());
    out.push(ulen);
    out.extend_from_slice(username);
    out.push(plen);
    out.extend_from_slice(password);
    Ok(out)
}

/// Run the server side up to and including reading the credentials, which
/// are returned as `(username, password)` for the caller to check.
pub(crate) async fn server_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    variant: Variant,
    pair: &KeyPair,
) -> Result<(Established, Vec<u8>), Error> {
    stream.write_all(&pair.public().encode()).await?;
    stream.flush().await?;
    let client_pub = read_public_key(stream).await?;
    let server_random = ra2::random().map_err(|e| fail(&e))?;
    write_encrypted_random(stream, &client_pub, &server_random).await?;
    let client_random = read_encrypted_random(stream, pair).await?;
    let (client_key, server_key) = ra2::derive_keys(variant, &server_random, &client_random);
    let mut cipher = Cipher::new(variant, &server_key, &client_key);

    let ours = ra2::exchange_hash(variant, pair.public(), &client_pub);
    write_message(stream, &mut cipher, &ours).await?;
    let client_hash = read_message(stream, &mut cipher).await?;
    if !same(
        &client_hash,
        &ra2::exchange_hash(variant, &client_pub, pair.public()),
    ) {
        return Err(fail(&Ra2Error::Authentication));
    }
    // Both a user name and a password: the only subtype offered.
    write_message(stream, &mut cipher, &[1]).await?;
    let creds = read_message(stream, &mut cipher).await?;
    let (username, rest) =
        split_len(&creds).ok_or_else(|| fail(&Ra2Error::BadLength("credentials")))?;
    let (password, rest) =
        split_len(rest).ok_or_else(|| fail(&Ra2Error::BadLength("credentials")))?;
    if !rest.is_empty() {
        return Err(fail(&Ra2Error::BadLength("credentials")));
    }
    Ok((
        Established {
            cipher,
            username: Some(username.to_vec()),
        },
        password.to_vec(),
    ))
}

/// A byte of length, then that many bytes; the rest.
fn split_len(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let (&len, rest) = data.split_first()?;
    (rest.len() >= usize::from(len)).then(|| rest.split_at(usize::from(len)))
}

/// The encrypted traffic after the handshake: every write becomes messages
/// of at most [`MAX_MESSAGE`] bytes, every read gives back what the messages
/// carried.
pub struct Ra2Stream<S> {
    inner: S,
    cipher: Cipher,
    /// Ciphertext read but not yet a whole message.
    raw: Vec<u8>,
    /// Decrypted bytes not yet handed to the reader.
    plain: Vec<u8>,
    plain_at: usize,
    /// Sealed messages not yet written to `inner`.
    out: Vec<u8>,
    out_at: usize,
}

impl<S> Ra2Stream<S> {
    pub(crate) fn new(inner: S, cipher: Cipher) -> Self {
        Self {
            inner,
            cipher,
            raw: Vec::new(),
            plain: Vec::new(),
            plain_at: 0,
            out: Vec::new(),
            out_at: 0,
        }
    }
}

impl<S> std::fmt::Debug for Ra2Stream<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ra2Stream").finish_non_exhaustive()
    }
}

fn invalid(e: Ra2Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl<S: AsyncRead + Unpin> AsyncRead for Ra2Stream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            if this.plain_at < this.plain.len() {
                let n = buf.remaining().min(this.plain.len() - this.plain_at);
                buf.put_slice(&this.plain[this.plain_at..this.plain_at + n]);
                this.plain_at += n;
                return Poll::Ready(Ok(()));
            }
            // A whole message?
            if this.raw.len() >= 2 {
                let len = usize::from(u16::from_be_bytes([this.raw[0], this.raw[1]]));
                if len > MAX_MESSAGE {
                    return Poll::Ready(Err(invalid(Ra2Error::BadLength("message"))));
                }
                let total = 2 + len + TAG_LEN;
                if this.raw.len() >= total {
                    let header = [this.raw[0], this.raw[1]];
                    let plain = this
                        .cipher
                        .open(header, &this.raw[2..total])
                        .map_err(invalid)?;
                    this.raw.drain(..total);
                    this.plain = plain;
                    this.plain_at = 0;
                    continue;
                }
            }
            let mut chunk = [0u8; 4096];
            let mut read = ReadBuf::new(&mut chunk);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut read))?;
            if read.filled().is_empty() {
                // The peer closed: fine between messages, an error inside one.
                return Poll::Ready(if this.raw.is_empty() {
                    Ok(())
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                });
            }
            this.raw.extend_from_slice(read.filled());
        }
    }
}

impl<S: AsyncWrite + Unpin> Ra2Stream<S> {
    /// Write out what is sealed and waiting.
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.out_at < self.out.len() {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &self.out[self.out_at..]))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.out_at += n;
        }
        self.out.clear();
        self.out_at = 0;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Ra2Stream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        // Take new data only once the previous messages are out, which bounds
        // what is buffered.
        ready!(this.poll_drain(cx))?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let take = buf.len().min(64 * 1024);
        for chunk in buf[..take].chunks(MAX_MESSAGE) {
            let sealed = this.cipher.seal(chunk).map_err(invalid)?;
            this.out.extend_from_slice(&sealed);
        }
        // Try to send it now; whatever is left goes out with the next call.
        if let Poll::Ready(Err(e)) = this.poll_drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(take))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}
