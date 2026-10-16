use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Error;

/// The stream plus the two buffers every session needs.
#[derive(Debug)]
pub(crate) struct Io<S> {
    pub stream: S,
    pub inbuf: BytesMut,
    pub outbuf: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Io<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            inbuf: BytesMut::with_capacity(16 * 1024),
            outbuf: Vec::new(),
        }
    }

    /// Read more bytes into `inbuf`. Cancel safe.
    pub async fn fill(&mut self) -> Result<(), Error> {
        if self.stream.read_buf(&mut self.inbuf).await? == 0 {
            return Err(Error::Closed);
        }
        Ok(())
    }

    /// Write and clear `outbuf`, if it holds anything.
    pub async fn flush(&mut self) -> Result<(), Error> {
        if !self.outbuf.is_empty() {
            self.stream.write_all(&self.outbuf).await?;
            self.stream.flush().await?;
            self.outbuf.clear();
        }
        Ok(())
    }
}
