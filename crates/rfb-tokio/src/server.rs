use core::time::Duration;
use std::sync::Arc;

use rfb_proto::{
    PixelFormat,
    conn::{EncodedRect, ServerConn, ServerConnEvent},
    consts::{security, vencrypt},
    handshake::{ServerConfig, ServerEvent},
    msg::{client::ClientMessage, server::ServerMessage},
};
use rfb_security::vnc_auth;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

use crate::{Error, HANDSHAKE_TIMEOUT, io::Io, transport::Transport};

type CheckFn = dyn Fn(&[u8], &[u8]) -> bool + Send + Sync;

/// Checks a username and password from `VeNCrypt` `X509Plain`.
#[derive(Clone)]
pub struct PlainVerifier(Arc<CheckFn>);

impl PlainVerifier {
    /// Wrap a function that returns whether the credentials are valid.
    pub fn new(check: impl Fn(&[u8], &[u8]) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }
}

impl std::fmt::Debug for PlainVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlainVerifier(..)")
    }
}

/// How the server authenticates a client.
#[derive(Debug, Clone)]
pub enum ServerAuth {
    /// Anyone may connect. Only for loopback, UNIX sockets or an already
    /// authenticated tunnel.
    None,
    /// VNC Auth with one shared password (weak: DES, 8 characters).
    VncPassword(Vec<u8>),
    /// A username and password checked by the callback. Only with TLS: the
    /// credentials travel inside the encrypted channel (`X509Plain`).
    Plain(PlainVerifier),
}

/// Settings for [`ServerSession::accept`].
#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub auth: ServerAuth,
    pub width: u16,
    pub height: u16,
    /// The server's native format, also the client's until it asks otherwise.
    pub pixel_format: PixelFormat,
    pub name: Vec<u8>,
    pub handshake_timeout: Duration,
    /// Offer `VeNCrypt` with this certificate. Without it the session stays
    /// unencrypted and `ServerAuth::Plain` is refused.
    pub tls: Option<Arc<rustls::ServerConfig>>,
}

impl ServerOptions {
    #[must_use]
    pub fn new(auth: ServerAuth, width: u16, height: u16, name: &[u8]) -> Self {
        Self {
            auth,
            width,
            height,
            pixel_format: PixelFormat::RGB888,
            name: name.to_vec(),
            handshake_timeout: HANDSHAKE_TIMEOUT,
            tls: None,
        }
    }

    /// Offer `VeNCrypt` TLS with `config`.
    #[must_use]
    pub fn with_tls(mut self, config: Arc<rustls::ServerConfig>) -> Self {
        self.tls = Some(config);
        self
    }
}

/// An established server-side connection to one client.
#[derive(Debug)]
pub struct ServerSession<S> {
    io: Io<Transport<S>>,
    conn: ServerConn,
    shared: bool,
    /// Who signed in with `X509Plain`.
    username: Option<Vec<u8>>,
    /// The VNC Auth challenge in flight.
    challenge: [u8; 16],
}

impl<S: AsyncRead + AsyncWrite + Unpin> ServerSession<S> {
    /// Run the handshake over `stream`; returns once `ServerInit` was sent.
    ///
    /// # Errors
    /// [`Error::HandshakeTimeout`], [`Error::AuthFailed`] (the client has been
    /// told), I/O and protocol errors.
    pub async fn accept(stream: S, opts: ServerOptions) -> Result<Self, Error> {
        let (security, subtype) = match (&opts.tls, &opts.auth) {
            (None, ServerAuth::None) => (security::NONE, None),
            (None, ServerAuth::VncPassword(_)) => (security::VNC_AUTH, None),
            (None, ServerAuth::Plain(_)) => {
                return Err(Error::Config("username and password need TLS"));
            }
            (Some(_), ServerAuth::None) => (security::VENCRYPT, Some(vencrypt::X509_NONE)),
            (Some(_), ServerAuth::VncPassword(_)) => (security::VENCRYPT, Some(vencrypt::X509_VNC)),
            (Some(_), ServerAuth::Plain(_)) => (security::VENCRYPT, Some(vencrypt::X509_PLAIN)),
        };
        let mut io = Io::new(Transport::Plain(stream));
        let conn = ServerConn::new(
            ServerConfig {
                security: vec![security],
                vencrypt: subtype.into_iter().collect(),
                ..ServerConfig::default()
            },
            &mut io.outbuf,
        );
        let mut session = Self {
            io,
            conn,
            shared: false,
            username: None,
            challenge: [0; 16],
        };
        timeout(opts.handshake_timeout, session.handshake(&opts))
            .await
            .map_err(|_| Error::HandshakeTimeout)??;
        Ok(session)
    }

    async fn handshake(&mut self, opts: &ServerOptions) -> Result<(), Error> {
        self.io.flush().await?; // the version message
        while !self.conn.is_established() {
            self.io.fill().await?;
            while let Some(event) = self.conn.poll(&mut self.io.inbuf, &mut self.io.outbuf)? {
                let ServerConnEvent::Handshake(event) = event else {
                    unreachable!("no client messages are parsed before server_init");
                };
                match event {
                    ServerEvent::VncAuthSelected => {
                        let mut challenge = [0u8; 16];
                        getrandom::fill(&mut challenge)
                            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
                        self.conn.send_challenge(&challenge, &mut self.io.outbuf)?;
                        self.io.flush().await?;
                        self.challenge = challenge;
                    }
                    ServerEvent::VncAuthResponse(response) => {
                        let ServerAuth::VncPassword(password) = &opts.auth else {
                            unreachable!("VNC Auth is only offered with a password");
                        };
                        let ok = vnc_auth::verify(password, &self.challenge, &response);
                        self.conn.security_result(
                            ok,
                            b"authentication failed",
                            &mut self.io.outbuf,
                        )?;
                        self.io.flush().await?;
                        if !ok {
                            return Err(Error::AuthFailed);
                        }
                    }
                    ServerEvent::StartTls { .. } => {
                        let config = opts
                            .tls
                            .clone()
                            .ok_or(Error::Config("TLS requested but no certificate"))?;
                        // The client starts TLS only after it has read our
                        // go-ahead byte, which is still in the output buffer.
                        self.io.flush().await?;
                        self.io.stream.upgrade_server(config).await?;
                        self.conn.tls_established(&mut self.io.outbuf)?;
                        self.io.flush().await?;
                    }
                    ServerEvent::PlainCredentials { username, password } => {
                        let ServerAuth::Plain(verifier) = &opts.auth else {
                            unreachable!("Plain credentials are only offered with a verifier");
                        };
                        // Checking a password hash is deliberately slow: keep it
                        // off the async worker threads.
                        let check = Arc::clone(&verifier.0);
                        let (user, pass) = (username.clone(), password);
                        let ok = tokio::task::spawn_blocking(move || check(&user, &pass))
                            .await
                            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
                        if ok {
                            self.username = Some(username.clone());
                        }
                        self.conn.security_result(
                            ok,
                            b"authentication failed",
                            &mut self.io.outbuf,
                        )?;
                        self.io.flush().await?;
                        if !ok {
                            return Err(Error::AuthFailed);
                        }
                    }
                    ServerEvent::ClientInit { shared } => {
                        self.shared = shared;
                        self.conn.server_init(
                            opts.width,
                            opts.height,
                            &opts.pixel_format,
                            &opts.name,
                            &mut self.io.outbuf,
                        )?;
                        self.io.flush().await?;
                        // Stop here: bytes the client already pipelined stay
                        // in the buffer for `next_message`.
                        return Ok(());
                    }
                }
            }
            self.io.flush().await?;
        }
        Ok(())
    }

    /// The client's `ClientInit` shared flag: `false` asks us to drop other
    /// clients.
    #[must_use]
    pub fn shared(&self) -> bool {
        self.shared
    }

    /// The user name the client signed in with (`X509Plain`), if it did.
    #[must_use]
    pub fn username(&self) -> Option<&[u8]> {
        self.username.as_deref()
    }

    /// Wait for the next client message. Cancel safe.
    ///
    /// # Errors
    /// [`Error::Closed`] when the client hangs up, plus I/O and protocol
    /// errors. After an error the session is dead.
    pub async fn next_message(&mut self) -> Result<ClientMessage, Error> {
        loop {
            match self.conn.poll(&mut self.io.inbuf, &mut self.io.outbuf)? {
                Some(ServerConnEvent::Message(msg)) => return Ok(msg),
                Some(ServerConnEvent::Handshake(_)) => unreachable!("handshake is over"),
                None => self.io.fill().await?,
            }
        }
    }

    /// Send a checked `FramebufferUpdate`; see [`ServerConn::send_update`].
    ///
    /// # Errors
    /// Protocol errors for a bad rectangle (nothing is sent), I/O errors.
    pub async fn send_update(&mut self, rects: &[EncodedRect<'_>]) -> Result<(), Error> {
        self.conn.send_update(rects, &mut self.io.outbuf)?;
        self.io.flush().await
    }

    /// Send `Bell`, `ServerCutText` or `SetColourMapEntries`.
    ///
    /// # Errors
    /// Protocol errors for an invalid message, I/O errors.
    pub async fn send(&mut self, msg: &ServerMessage) -> Result<(), Error> {
        self.conn.send(msg, &mut self.io.outbuf)?;
        self.io.flush().await
    }

    /// Whether the session runs inside TLS.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.io.stream.is_tls()
    }

    /// Bytes read from the stream so far, handshake included.
    #[must_use]
    pub fn bytes_received(&self) -> u64 {
        self.io.received
    }

    /// Bytes written to the stream so far, handshake included.
    #[must_use]
    pub fn bytes_sent(&self) -> u64 {
        self.io.sent
    }

    /// The protocol state: client pixel format and encodings, size, version.
    #[must_use]
    pub fn conn(&self) -> &ServerConn {
        &self.conn
    }
}
