use core::time::Duration;

use rfb_proto::{
    PixelFormat,
    conn::{ClientConn, ClientConnEvent},
    handshake::{ClientConfig, ClientEvent},
    msg::client::ClientMessage,
};
use rfb_security::vnc_auth;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

use crate::{Error, HANDSHAKE_TIMEOUT, io::Io};

/// Settings for [`ClientSession::connect`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub handshake: ClientConfig,
    /// Password for VNC Auth. `None` fails with [`Error::PasswordRequired`]
    /// if the server asks for one.
    pub password: Option<Vec<u8>>,
    pub handshake_timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            handshake: ClientConfig::default(),
            password: None,
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
    io: Io<S>,
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
        let mut session = Self {
            io: Io::new(stream),
            conn: ClientConn::new(opts.handshake.clone()),
        };
        let info = timeout(
            opts.handshake_timeout,
            session.handshake(opts.password.as_deref()),
        )
        .await
        .map_err(|_| Error::HandshakeTimeout)??;
        Ok((session, info))
    }

    async fn handshake(&mut self, password: Option<&[u8]>) -> Result<ServerInfo, Error> {
        loop {
            match self.next_event().await? {
                ClientConnEvent::Handshake(ClientEvent::VncAuthChallenge(challenge)) => {
                    let password = password.ok_or(Error::PasswordRequired)?;
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
