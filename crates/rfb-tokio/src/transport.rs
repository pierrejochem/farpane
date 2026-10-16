//! A stream that starts in the clear and can be upgraded to TLS in place,
//! which is what `VeNCrypt` needs: the RFB handshake begins unencrypted and
//! the rest of the session continues inside TLS on the same connection.

use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The session's stream.
pub enum Transport<S> {
    Plain(S),
    TlsClient(Box<tokio_rustls::client::TlsStream<S>>),
    TlsServer(Box<tokio_rustls::server::TlsStream<S>>),
    /// RSA-AES encrypted (the stream after an RA2 handshake).
    Ra2(Box<crate::ra2::Ra2Stream<S>>),
    /// While the stream is handed to TLS. Using it then is an error, and so is
    /// any use after a failed upgrade (the session is dead by then).
    Upgrading,
}

impl<S> std::fmt::Debug for Transport<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Plain(_) => "Transport::Plain",
            Self::TlsClient(_) => "Transport::TlsClient",
            Self::TlsServer(_) => "Transport::TlsServer",
            Self::Ra2(_) => "Transport::Ra2",
            Self::Upgrading => "Transport::Upgrading",
        })
    }
}

impl<S> Transport<S> {
    /// True once the stream runs inside TLS.
    pub fn is_tls(&self) -> bool {
        matches!(self, Self::TlsClient(_) | Self::TlsServer(_))
    }

    /// True while the stream runs inside RSA-AES encryption.
    pub fn is_ra2(&self) -> bool {
        matches!(self, Self::Ra2(_))
    }
}

fn upgrading() -> io::Error {
    io::Error::other("the connection is being upgraded to TLS")
}

impl<S: AsyncRead + AsyncWrite + Unpin> Transport<S> {
    /// Run the RSA-AES handshake as the client and, if `keep_encrypted`,
    /// leave the stream encrypted with the session keys.
    pub(crate) async fn ra2_client(
        &mut self,
        variant: rfb_security::ra2::Variant,
        keep_encrypted: bool,
        opts: &crate::ra2::Ra2Options,
        username: Option<&[u8]>,
        password: &[u8],
    ) -> Result<(), crate::Error> {
        let Self::Plain(stream) = self else {
            return Err(crate::Error::Config("RSA-AES needs the plain stream"));
        };
        let done = crate::ra2::client_handshake(stream, variant, opts, username, password).await?;
        self.finish_ra2(done.cipher, keep_encrypted)
    }

    /// Run the RSA-AES handshake as the server. Returns the user name and
    /// password the client sent; the stream is then encrypted, or plain again
    /// for the `ne` types.
    pub(crate) async fn ra2_server(
        &mut self,
        variant: rfb_security::ra2::Variant,
        keep_encrypted: bool,
        pair: &rfb_security::ra2::KeyPair,
    ) -> Result<(Vec<u8>, Vec<u8>), crate::Error> {
        let Self::Plain(stream) = self else {
            return Err(crate::Error::Config("RSA-AES needs the plain stream"));
        };
        let (done, password) = crate::ra2::server_handshake(stream, variant, pair).await?;
        let username = done.username.unwrap_or_default();
        self.finish_ra2(done.cipher, keep_encrypted)?;
        Ok((username, password))
    }

    fn finish_ra2(
        &mut self,
        cipher: rfb_security::ra2::Cipher,
        keep_encrypted: bool,
    ) -> Result<(), crate::Error> {
        if keep_encrypted {
            let Self::Plain(stream) = std::mem::replace(self, Self::Upgrading) else {
                return Err(crate::Error::Config("RSA-AES needs the plain stream"));
            };
            *self = Self::Ra2(Box::new(crate::ra2::Ra2Stream::new(stream, cipher)));
        }
        Ok(())
    }

    /// Run the TLS handshake as the client.
    ///
    /// # Errors
    /// The handshake error. The transport is unusable afterwards.
    pub async fn upgrade_client(
        &mut self,
        config: Arc<rustls::ClientConfig>,
        name: ServerName<'static>,
    ) -> io::Result<()> {
        let Self::Plain(stream) = std::mem::replace(self, Self::Upgrading) else {
            return Err(io::Error::other("TLS was already started"));
        };
        let tls = tokio_rustls::TlsConnector::from(config)
            .connect(name, stream)
            .await?;
        *self = Self::TlsClient(Box::new(tls));
        Ok(())
    }

    /// Run the TLS handshake as the server.
    ///
    /// # Errors
    /// The handshake error. The transport is unusable afterwards.
    pub async fn upgrade_server(&mut self, config: Arc<rustls::ServerConfig>) -> io::Result<()> {
        let Self::Plain(stream) = std::mem::replace(self, Self::Upgrading) else {
            return Err(io::Error::other("TLS was already started"));
        };
        let tls = tokio_rustls::TlsAcceptor::from(config)
            .accept(stream)
            .await?;
        *self = Self::TlsServer(Box::new(tls));
        Ok(())
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for Transport<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::TlsClient(s) => Pin::new(s).poll_read(cx, buf),
            Self::TlsServer(s) => Pin::new(s).poll_read(cx, buf),
            Self::Ra2(s) => Pin::new(s).poll_read(cx, buf),
            Self::Upgrading => Poll::Ready(Err(upgrading())),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Transport<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::TlsClient(s) => Pin::new(s).poll_write(cx, buf),
            Self::TlsServer(s) => Pin::new(s).poll_write(cx, buf),
            Self::Ra2(s) => Pin::new(s).poll_write(cx, buf),
            Self::Upgrading => Poll::Ready(Err(upgrading())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::TlsClient(s) => Pin::new(s).poll_flush(cx),
            Self::TlsServer(s) => Pin::new(s).poll_flush(cx),
            Self::Ra2(s) => Pin::new(s).poll_flush(cx),
            Self::Upgrading => Poll::Ready(Err(upgrading())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::TlsClient(s) => Pin::new(s).poll_shutdown(cx),
            Self::TlsServer(s) => Pin::new(s).poll_shutdown(cx),
            Self::Ra2(s) => Pin::new(s).poll_shutdown(cx),
            Self::Upgrading => Poll::Ready(Err(upgrading())),
        }
    }
}
