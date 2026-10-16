use core::time::Duration;

use rfb_proto::{
    PixelFormat,
    conn::{EncodedRect, ServerConn, ServerConnEvent},
    consts::security,
    handshake::{ServerConfig, ServerEvent},
    msg::{client::ClientMessage, server::ServerMessage},
};
use rfb_security::vnc_auth;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

use crate::{Error, HANDSHAKE_TIMEOUT, io::Io};

/// How the server authenticates a client.
#[derive(Debug, Clone)]
pub enum ServerAuth {
    /// Anyone may connect. Only for loopback, UNIX sockets or an already
    /// authenticated tunnel.
    None,
    /// VNC Auth with one shared password (weak: DES, 8 characters).
    VncPassword(Vec<u8>),
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
        }
    }
}

/// An established server-side connection to one client.
#[derive(Debug)]
pub struct ServerSession<S> {
    io: Io<S>,
    conn: ServerConn,
    shared: bool,
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
        let sec = match opts.auth {
            ServerAuth::None => security::NONE,
            ServerAuth::VncPassword(_) => security::VNC_AUTH,
        };
        let mut io = Io::new(stream);
        let conn = ServerConn::new(
            ServerConfig {
                security: vec![sec],
                ..ServerConfig::default()
            },
            &mut io.outbuf,
        );
        let mut session = Self {
            io,
            conn,
            shared: false,
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
                    // This server never offers VeNCrypt, so these cannot
                    // happen; a peer that somehow provokes one gets an error.
                    ServerEvent::StartTls { .. } | ServerEvent::PlainCredentials { .. } => {
                        return Err(Error::Proto(rfb_proto::ProtoError::NoAcceptableSecurity));
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
