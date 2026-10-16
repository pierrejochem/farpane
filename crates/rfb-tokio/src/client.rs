use core::time::Duration;

use rfb_proto::{
    PixelFormat,
    conn::{ClientConn, ClientConnEvent},
    consts::{security, vencrypt},
    handshake::{ClientConfig, ClientEvent},
    msg::client::ClientMessage,
};
use rfb_security::{
    tls::{self, Fingerprint},
    vnc_auth,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

use crate::{Error, HANDSHAKE_TIMEOUT, io::Io, transport::Transport};

/// Settings for [`ClientSession::connect`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub handshake: ClientConfig,
    /// Password for VNC Auth. `None` fails with [`Error::PasswordRequired`]
    /// if the server asks for one.
    pub password: Option<Vec<u8>>,
    /// Username for `VeNCrypt` `X509Plain`. `None` fails with
    /// [`Error::UsernameRequired`] if the server asks for one.
    pub username: Option<Vec<u8>>,
    /// TLS settings. Without them `VeNCrypt` is never chosen, so a server
    /// that offers nothing else is refused rather than talked to in the clear.
    pub tls: Option<TlsOptions>,
    /// RSA-AES settings. Without them RA2 types are never chosen (the
    /// server's key must be pinned, so the caller has to be able to ask).
    pub ra2: Option<crate::Ra2Options>,
    /// Refuse any server that does not offer an encrypted type (`VeNCrypt`
    /// or RSA-AES). Set it when the user already trusts this server's key:
    /// otherwise someone on the path could answer "None" or "VNC Auth" and
    /// the connection would silently fall back to the clear.
    pub require_encryption: bool,
    pub handshake_timeout: Duration,
}

/// How to treat the server's certificate (see [`rfb_security::tls`]).
#[derive(Debug, Clone)]
pub struct TlsOptions {
    /// The name the certificate must be valid for: the host the user typed.
    pub server_name: String,
    /// Fingerprints the user trusts for this server.
    pub pins: Vec<Fingerprint>,
    /// DER certificates of additional CAs to trust.
    pub extra_roots: Vec<Vec<u8>>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            handshake: ClientConfig::default(),
            password: None,
            username: None,
            tls: None,
            ra2: None,
            require_encryption: false,
            handshake_timeout: HANDSHAKE_TIMEOUT,
        }
    }
}

/// What the server announced in `ServerInit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub width: u16,
    pub height: u16,
    pub pixel_format: PixelFormat,
    /// Desktop name; arbitrary bytes, usually UTF-8.
    pub name: Vec<u8>,
}

/// An established client connection.
#[derive(Debug)]
pub struct ClientSession<S> {
    io: Io<Transport<S>>,
    conn: ClientConn,
}

impl<S: AsyncRead + AsyncWrite + Unpin> ClientSession<S> {
    /// Run the handshake over `stream` and return the session once the server
    /// has sent `ServerInit`.
    ///
    /// # Errors
    /// [`Error::HandshakeTimeout`], [`Error::PasswordRequired`], I/O and
    /// protocol errors (including the server's rejection reason).
    pub async fn connect(stream: S, opts: ClientOptions) -> Result<(Self, ServerInfo), Error> {
        let mut config = opts.handshake.clone();
        if opts.tls.is_none() {
            config.security.retain(|t| *t != security::VENCRYPT);
        }
        if opts.require_encryption {
            config.security.retain(|t| {
                matches!(
                    *t,
                    security::VENCRYPT
                        | security::RA2
                        | security::RA2NE
                        | security::RA2_256
                        | security::RA2NE_256
                )
            });
        }
        if opts.ra2.is_none() || opts.password.is_none() {
            config.security.retain(|t| {
                !matches!(
                    *t,
                    security::RA2 | security::RA2NE | security::RA2_256 | security::RA2NE_256
                )
            });
        }
        // Prefer the subtype that matches what the user supplied.
        config.vencrypt = match (&opts.username, &opts.password) {
            (Some(_), _) => vec![
                vencrypt::X509_PLAIN,
                vencrypt::X509_VNC,
                vencrypt::X509_NONE,
            ],
            (None, Some(_)) => vec![
                vencrypt::X509_VNC,
                vencrypt::X509_NONE,
                vencrypt::X509_PLAIN,
            ],
            (None, None) => vec![
                vencrypt::X509_NONE,
                vencrypt::X509_VNC,
                vencrypt::X509_PLAIN,
            ],
        };
        let mut session = Self {
            io: Io::new(Transport::Plain(stream)),
            conn: ClientConn::new(config),
        };
        let info = timeout(opts.handshake_timeout, session.handshake(&opts))
            .await
            .map_err(|_| Error::HandshakeTimeout)??;
        Ok((session, info))
    }

    async fn handshake(&mut self, opts: &ClientOptions) -> Result<ServerInfo, Error> {
        loop {
            match self.next_event().await? {
                ClientConnEvent::Handshake(ClientEvent::StartTls { .. }) => {
                    let tls = opts
                        .tls
                        .as_ref()
                        .ok_or(Error::Config("TLS requested but no TLS options"))?;
                    self.start_tls(tls).await?;
                    self.conn.tls_established()?;
                }
                ClientConnEvent::Handshake(ClientEvent::StartRa2 { security: chosen }) => {
                    let (variant, keep) = rfb_security::ra2::Variant::from_security(chosen)
                        .ok_or(Error::Config("unsupported RSA-AES type"))?;
                    let ra2 = opts
                        .ra2
                        .as_ref()
                        .ok_or(Error::Config("RSA-AES requested but no RSA-AES options"))?;
                    let password = opts.password.as_deref().ok_or(Error::PasswordRequired)?;
                    self.io
                        .stream
                        .ra2_client(variant, keep, ra2, opts.username.as_deref(), password)
                        .await?;
                    self.conn.ra2_established()?;
                }
                ClientConnEvent::Handshake(ClientEvent::PlainCredentialsNeeded) => {
                    let username = opts.username.as_deref().ok_or(Error::UsernameRequired)?;
                    let password = opts.password.as_deref().ok_or(Error::PasswordRequired)?;
                    self.conn
                        .plain_credentials(username, password, &mut self.io.outbuf)?;
                    self.io.flush().await?;
                }
                ClientConnEvent::Handshake(ClientEvent::VncAuthChallenge(challenge)) => {
                    let password = opts.password.as_deref().ok_or(Error::PasswordRequired)?;
                    let response = vnc_auth::response(password, &challenge);
                    self.conn.vnc_auth_reply(&response, &mut self.io.outbuf)?;
                    self.io.flush().await?;
                }
                ClientConnEvent::Handshake(ClientEvent::ServerInit {
                    width,
                    height,
                    pixel_format,
                    name,
                }) => {
                    return Ok(ServerInfo {
                        width,
                        height,
                        pixel_format,
                        name,
                    });
                }
                other => {
                    unreachable!("the connection yields only handshake events here: {other:?}")
                }
            }
        }
    }

    /// Upgrade the stream to TLS. A certificate that is not trusted comes back
    /// as [`Error::Untrusted`] with its fingerprint, so the caller can ask.
    async fn start_tls(&mut self, opts: &TlsOptions) -> Result<(), Error> {
        let (config, finding) =
            tls::client_config(opts.pins.clone(), &opts.extra_roots).map_err(Error::Tls)?;
        let name = rustls::pki_types::ServerName::try_from(opts.server_name.clone())
            .map_err(|e| Error::Tls(format!("bad server name {:?}: {e}", opts.server_name)))?;
        match self.io.stream.upgrade_client(config, name).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let seen = finding.lock().ok().and_then(|f| f.clone());
                Err(match seen {
                    Some(u) => Error::Untrusted {
                        host: opts.server_name.clone(),
                        fingerprint: u.fingerprint,
                        problem: u.problem,
                    },
                    None => Error::Tls(e.to_string()),
                })
            }
        }
    }

    /// Wait for the next event. Cancel safe.
    ///
    /// # Errors
    /// [`Error::Closed`] when the server hangs up, plus I/O and protocol
    /// errors. After an error the session is dead.
    pub async fn next_event(&mut self) -> Result<ClientConnEvent, Error> {
        loop {
            let event = self.conn.poll(&mut self.io.inbuf, &mut self.io.outbuf)?;
            self.io.flush().await?;
            if let Some(event) = event {
                return Ok(event);
            }
            self.io.fill().await?;
        }
    }

    /// Send a message and flush. Await to completion; see the crate docs.
    ///
    /// # Errors
    /// Protocol errors for an invalid message, I/O errors.
    pub async fn send(&mut self, msg: &ClientMessage) -> Result<(), Error> {
        self.conn.send(msg, &mut self.io.outbuf)?;
        self.io.flush().await
    }

    /// Whether the session runs inside RSA-AES encryption.
    pub fn is_ra2(&self) -> bool {
        self.io.stream.is_ra2()
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

    /// The protocol state: framebuffer size, pixel format, version.
    #[must_use]
    pub fn conn(&self) -> &ClientConn {
        &self.conn
    }
}
