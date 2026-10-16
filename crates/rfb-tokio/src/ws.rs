//! WebSocket (RFC 6455) as a byte stream, so that browsers (noVNC) can reach
//! the server: [`accept`] answers the HTTP upgrade and returns a [`WsStream`]
//! that carries the RFB bytes in binary frames and implements
//! `AsyncRead + AsyncWrite`.
//!
//! Only what RFB over WebSocket needs: binary and continuation frames, ping
//! and pong, close. Text frames, extensions (no compression) and anything
//! malformed end the connection. Every length is bounded.

use std::{
    fmt::Write as _,
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, BufMut, BytesMut};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// The request (line and headers) may not be longer than this.
const MAX_REQUEST: usize = 8 * 1024;
/// The largest frame payload accepted. RFB messages are bounded far below.
const MAX_FRAME: usize = 1 << 20;
/// How much of a write goes into one frame.
const WRITE_FRAME: usize = 64 * 1024;
/// RFC 6455, section 1.3.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

fn bad(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Which pages may open a WebSocket to the server. Browsers always send an
/// `Origin`; a malicious page could otherwise use a visitor's browser to reach
/// a server that is only listening on their own machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OriginPolicy {
    /// The page must come from the host the browser connected to (the
    /// default). Clients that send no `Origin` (not browsers) are let in.
    #[default]
    SameHost,
    /// Any page may connect (the server's own sign-in still applies).
    Any,
    /// Only these origins, such as `https://desktop.example.org`.
    List(Vec<String>),
}

/// The value the server must answer `Sec-WebSocket-Key` with.
#[must_use]
pub fn accept_key(key: &str) -> String {
    use base64::Engine as _;
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Whether `origin` is allowed by `policy` for a request to `host`.
fn origin_allowed(policy: &OriginPolicy, origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    match policy {
        OriginPolicy::Any => true,
        OriginPolicy::List(list) => list.iter().any(|o| o.eq_ignore_ascii_case(origin)),
        OriginPolicy::SameHost => {
            let after_scheme = origin.split_once("://").map_or(origin, |(_, rest)| rest);
            host.is_some_and(|h| after_scheme.eq_ignore_ascii_case(h))
        }
    }
}

async fn respond(stream: &mut (impl AsyncWrite + Unpin), status: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(text.as_bytes()).await;
    let _ = stream.flush().await;
}

/// Read the HTTP upgrade request from `stream`, check it and answer with
/// `101 Switching Protocols`.
///
/// # Errors
/// I/O errors and malformed or refused requests (the client is told with an
/// HTTP error first where it makes sense).
pub async fn accept<S>(mut stream: S, origins: &OriginPolicy) -> io::Result<WsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut raw = BytesMut::with_capacity(1024);
    let end = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if raw.len() >= MAX_REQUEST {
            respond(
                &mut stream,
                "431 Request Header Fields Too Large",
                "too large",
            )
            .await;
            return Err(bad("WebSocket request too large"));
        }
        if stream.read_buf(&mut raw).await? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
    };
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    // Bytes after the headers belong to the frames that follow.
    let rest = raw.split_off(end);
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split(' ');
    let (method, version) = (parts.next(), parts.nth(1));
    let headers: Vec<(String, String)> = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let wants_upgrade = header(&headers, "Upgrade")
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        && header(&headers, "Connection").is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });
    if method != Some("GET") || version != Some("HTTP/1.1") || !wants_upgrade {
        respond(
            &mut stream,
            "426 Upgrade Required",
            "This is a Farpane WebSocket endpoint; connect with a WebSocket client such as noVNC.",
        )
        .await;
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not a WebSocket upgrade request ({request:?})"),
        ));
    }
    if header(&headers, "Sec-WebSocket-Version") != Some("13") {
        let _ = stream
            .write_all(
                b"HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
        return Err(bad("unsupported WebSocket version"));
    }
    let Some(key) = header(&headers, "Sec-WebSocket-Key").filter(|k| k.len() == 24) else {
        respond(&mut stream, "400 Bad Request", "missing Sec-WebSocket-Key").await;
        return Err(bad("missing Sec-WebSocket-Key"));
    };
    if !origin_allowed(
        origins,
        header(&headers, "Origin"),
        header(&headers, "Host"),
    ) {
        respond(
            &mut stream,
            "403 Forbidden",
            "this page may not use this server",
        )
        .await;
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "WebSocket origin refused",
        ));
    }
    let offers_binary = header(&headers, "Sec-WebSocket-Protocol").is_some_and(|v| {
        v.split(',')
            .any(|p| p.trim().eq_ignore_ascii_case("binary"))
    });
    let mut reply = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n",
        accept_key(key)
    );
    if offers_binary {
        reply.push_str("Sec-WebSocket-Protocol: binary\r\n");
    }
    reply.push_str("\r\n");
    stream.write_all(reply.as_bytes()).await?;
    stream.flush().await?;
    Ok(WsStream::new(stream, Role::Server, rest))
}

/// Run the TLS handshake on `stream` as the server (for `wss`).
///
/// # Errors
/// I/O and TLS errors.
pub async fn accept_tls<S>(
    stream: S,
    config: std::sync::Arc<rustls::ServerConfig>,
) -> io::Result<tokio_rustls::server::TlsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio_rustls::TlsAcceptor::from(config).accept(stream).await
}

/// Which end of the connection this is: it decides who masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Reads masked frames, writes unmasked ones.
    Server,
    /// Reads unmasked frames, writes masked ones (used by tests and tools).
    Client,
}

/// A WebSocket connection as a byte stream. Everything written is sent as
/// binary frames; the payload of every data frame received is read back.
#[derive(Debug)]
pub struct WsStream<S> {
    inner: S,
    role: Role,
    /// Bytes read from `inner` and not yet parsed.
    inbuf: BytesMut,
    /// Payload ready to be read.
    ready: BytesMut,
    /// Inside a fragmented message.
    in_message: bool,
    /// Frame bytes waiting to go to `inner`.
    outbuf: BytesMut,
    /// The peer has closed: reads end.
    closed: bool,
    close_sent: bool,
}

struct Frame {
    opcode: u8,
    fin: bool,
    payload: Vec<u8>,
}

impl<S> WsStream<S> {
    /// Wrap `inner` after the HTTP upgrade; `pending` is whatever was read
    /// past the end of the HTTP headers.
    #[must_use]
    pub fn new(inner: S, role: Role, pending: BytesMut) -> Self {
        Self {
            inner,
            role,
            inbuf: pending,
            ready: BytesMut::new(),
            in_message: false,
            outbuf: BytesMut::new(),
            closed: false,
            close_sent: false,
        }
    }

    fn queue_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        self.outbuf.put_u8(0x80 | opcode);
        let mask_bit = if self.role == Role::Client { 0x80 } else { 0 };
        let len = payload.len();
        if let Some(short) = u8::try_from(len).ok().filter(|&n| n < 126) {
            self.outbuf.put_u8(mask_bit | short);
        } else if let Ok(medium) = u16::try_from(len) {
            self.outbuf.put_u8(mask_bit | 0x7E);
            self.outbuf.put_u16(medium);
        } else {
            self.outbuf.put_u8(mask_bit | 0x7F);
            self.outbuf.put_u64(len as u64);
        }
        if self.role == Role::Client {
            let mut key = [0u8; 4];
            getrandom::fill(&mut key).map_err(io::Error::other)?;
            self.outbuf.put_slice(&key);
            self.outbuf
                .extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        } else {
            self.outbuf.put_slice(payload);
        }
        Ok(())
    }

    /// Take one complete frame from `inbuf`, if there is one.
    fn parse_frame(&mut self) -> io::Result<Option<Frame>> {
        let buf = &self.inbuf;
        if buf.len() < 2 {
            return Ok(None);
        }
        let (b0, b1) = (buf[0], buf[1]);
        if b0 & 0x70 != 0 {
            return Err(bad("WebSocket extensions are not supported"));
        }
        let masked = b1 & 0x80 != 0;
        if masked != (self.role == Role::Server) {
            return Err(bad(if masked {
                "a server must not mask its frames"
            } else {
                "a client must mask its frames"
            }));
        }
        let (len, mut at) = match b1 & 0x7F {
            126 => {
                if buf.len() < 4 {
                    return Ok(None);
                }
                (usize::from(u16::from_be_bytes([buf[2], buf[3]])), 4)
            }
            127 => {
                if buf.len() < 10 {
                    return Ok(None);
                }
                let mut eight = [0u8; 8];
                eight.copy_from_slice(&buf[2..10]);
                let n = u64::from_be_bytes(eight);
                (usize::try_from(n).map_err(|_| bad("frame too large"))?, 10)
            }
            n => (usize::from(n), 2),
        };
        let opcode = b0 & 0x0F;
        if len > MAX_FRAME {
            return Err(bad("WebSocket frame too large"));
        }
        if opcode >= 0x8 && (len > 125 || b0 & 0x80 == 0) {
            return Err(bad("bad WebSocket control frame"));
        }
        let mut key = [0u8; 4];
        if masked {
            if buf.len() < at + 4 {
                return Ok(None);
            }
            key.copy_from_slice(&buf[at..at + 4]);
            at += 4;
        }
        if buf.len() < at + len {
            return Ok(None);
        }
        let mut payload = buf[at..at + len].to_vec();
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= key[i % 4];
            }
        }
        let fin = b0 & 0x80 != 0;
        self.inbuf.advance(at + len);
        Ok(Some(Frame {
            opcode,
            fin,
            payload,
        }))
    }

    /// Act on a parsed frame.
    fn handle(&mut self, frame: &Frame) -> io::Result<()> {
        match frame.opcode {
            OP_BINARY if !self.in_message => {
                self.ready.extend_from_slice(&frame.payload);
                self.in_message = !frame.fin;
            }
            OP_CONTINUATION if self.in_message => {
                self.ready.extend_from_slice(&frame.payload);
                self.in_message = !frame.fin;
            }
            OP_BINARY | OP_CONTINUATION => return Err(bad("WebSocket fragments out of order")),
            OP_TEXT => return Err(bad("RFB over WebSocket uses binary frames, not text")),
            OP_PING => self.queue_frame(OP_PONG, &frame.payload)?,
            OP_PONG => {}
            OP_CLOSE => {
                if !self.close_sent {
                    // Echo the status code, as the RFC asks.
                    let code = frame.payload.get(..2).unwrap_or(&[]).to_vec();
                    self.queue_frame(OP_CLOSE, &code)?;
                    self.close_sent = true;
                }
                self.closed = true;
            }
            _ => return Err(bad("unknown WebSocket opcode")),
        }
        Ok(())
    }
}

impl<S: AsyncWrite + Unpin> WsStream<S> {
    /// Push queued frame bytes to `inner`.
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while !self.outbuf.is_empty() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.outbuf) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
                Poll::Ready(Ok(n)) => self.outbuf.advance(n),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for WsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            if !this.ready.is_empty() {
                let n = this.ready.len().min(out.remaining());
                out.put_slice(&this.ready[..n]);
                this.ready.advance(n);
                return Poll::Ready(Ok(()));
            }
            // A pong or close reply may be waiting; sending it is best effort
            // here and finished by the next write or flush.
            if !this.outbuf.is_empty() {
                if let Poll::Ready(Err(e)) = this.drain(cx) {
                    return Poll::Ready(Err(e));
                }
            }
            if this.closed {
                return Poll::Ready(Ok(()));
            }
            if let Some(frame) = this.parse_frame()? {
                this.handle(&frame)?;
                continue;
            }
            if this.inbuf.len() > MAX_FRAME + 14 {
                return Poll::Ready(Err(bad("WebSocket frame too large")));
            }
            let mut chunk = [0u8; 8192];
            let mut read = ReadBuf::new(&mut chunk);
            match Pin::new(&mut this.inner).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {
                    if read.filled().is_empty() {
                        // The transport ended without a close frame.
                        this.closed = true;
                    } else {
                        this.inbuf.extend_from_slice(read.filled());
                    }
                }
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for WsStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // One frame in flight at a time keeps the queue (and memory) bounded.
        if this.drain(cx).is_pending() {
            return Poll::Pending;
        }
        if this.close_sent {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let n = buf.len().min(WRITE_FRAME);
        this.queue_frame(OP_BINARY, &buf[..n])?;
        if let Poll::Ready(Err(e)) = this.drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if let Poll::Ready(Err(e)) = this.drain(cx) {
            return Poll::Ready(Err(e));
        }
        if !this.outbuf.is_empty() {
            return Poll::Pending;
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if !this.close_sent {
            this.queue_frame(OP_CLOSE, &1000u16.to_be_bytes())?;
            this.close_sent = true;
        }
        if let Poll::Ready(Err(e)) = this.drain(cx) {
            return Poll::Ready(Err(e));
        }
        if !this.outbuf.is_empty() {
            return Poll::Pending;
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// Make a client-side WebSocket over `stream` by sending the upgrade request
/// (used by tests and tools; browsers do this themselves).
///
/// # Errors
/// I/O errors and a refused upgrade.
pub async fn connect<S>(mut stream: S, host: &str, origin: Option<&str>) -> io::Result<WsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use base64::Engine as _;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let key = base64::engine::general_purpose::STANDARD.encode(nonce);
    let mut request = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: binary\r\n"
    );
    if let Some(origin) = origin {
        let _ = write!(request, "Origin: {origin}\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = BytesMut::new();
    let end = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if raw.len() >= MAX_REQUEST {
            return Err(bad("WebSocket response too large"));
        }
        if stream.read_buf(&mut raw).await? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
    };
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let rest = raw.split_off(end);
    if !head.starts_with("HTTP/1.1 101") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            head.lines().next().unwrap_or("refused").to_owned(),
        ));
    }
    let expected = accept_key(&key);
    let ok = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .any(|(k, v)| {
            k.trim().eq_ignore_ascii_case("Sec-WebSocket-Accept") && v.trim() == expected
        });
    if !ok {
        return Err(bad("wrong Sec-WebSocket-Accept"));
    }
    Ok(WsStream::new(stream, Role::Client, rest))
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    /// A WebSocket pair over an in-memory pipe.
    async fn pair() -> (
        WsStream<tokio::io::DuplexStream>,
        WsStream<tokio::io::DuplexStream>,
    ) {
        let (a, b) = duplex(1 << 16);
        let server = tokio::spawn(async move { accept(b, &OriginPolicy::default()).await });
        let client = connect(a, "example.test", None).await.unwrap();
        (client, server.await.unwrap().unwrap())
    }

    #[test]
    fn the_accept_key_matches_the_example_in_the_rfc() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[tokio::test]
    async fn bytes_travel_both_ways_in_any_size() {
        let (mut client, mut server) = pair().await;
        // Larger than one frame, and than the pipe, so both sides must overlap.
        let data: Vec<u8> = (0..300_000u32)
            .map(|i| i.wrapping_mul(7).to_le_bytes()[0])
            .collect();
        let sent = data.clone();
        let writer = tokio::spawn(async move {
            client.write_all(&sent).await.unwrap();
            client.flush().await.unwrap();
            let mut echo = vec![0u8; 5];
            client.read_exact(&mut echo).await.unwrap();
            (client, echo)
        });
        let mut got = vec![0u8; data.len()];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(got, data);
        server.write_all(b"hello").await.unwrap();
        server.flush().await.unwrap();
        let (_client, echo) = writer.await.unwrap();
        assert_eq!(echo, b"hello");
    }

    #[tokio::test]
    async fn the_server_reads_fragmented_messages_ping_and_close() {
        let (raw, b) = duplex(1 << 16);
        let mut raw = raw;
        // Hand-made masked frames: two fragments, a ping between them.
        let mask = [1u8, 2, 3, 4];
        let frame = |b0: u8, payload: &[u8]| {
            let mut f = vec![b0, 0x80 | u8::try_from(payload.len()).unwrap()];
            f.extend_from_slice(&mask);
            f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            f
        };
        let mut server = WsStream::new(b, Role::Server, BytesMut::new());
        raw.write_all(&frame(OP_BINARY, b"abc")).await.unwrap(); // fin = 0
        raw.write_all(&frame(0x80 | OP_PING, b"ping"))
            .await
            .unwrap();
        raw.write_all(&frame(0x80 | OP_CONTINUATION, b"def"))
            .await
            .unwrap();
        raw.write_all(&frame(0x80 | OP_CLOSE, &1000u16.to_be_bytes()))
            .await
            .unwrap();
        let mut text = String::new();
        server.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "abcdef");
        // The ping was answered with a pong and the close with a close.
        let mut reply = vec![0u8; 64];
        let n = raw.read(&mut reply).await.unwrap();
        assert_eq!(
            &reply[..n],
            [
                &[0x80 | OP_PONG, 4][..],
                b"ping",
                &[0x80 | OP_CLOSE, 2, 3, 0xE8]
            ]
            .concat()
        );
    }

    #[tokio::test]
    async fn malformed_frames_end_the_connection() {
        for (name, bytes) in [
            ("unmasked frame from a client", vec![0x82, 0x01, 0x00]),
            ("reserved bit", vec![0xC2, 0x81, 0, 0, 0, 0, 0]),
            ("text frame", vec![0x81, 0x81, 0, 0, 0, 0, b'x']),
            ("oversized", {
                let mut v = vec![0x82, 0xFF];
                v.extend_from_slice(&(u64::MAX).to_be_bytes());
                v.extend_from_slice(&[0, 0, 0, 0]);
                v
            }),
            (
                "control frame too long",
                vec![0x89, 0xFE, 0x00, 0x7E, 0, 0, 0, 0],
            ),
            ("fragmented ping", vec![0x09, 0x80, 0, 0, 0, 0]),
            ("continuation without start", vec![0x80, 0x80, 0, 0, 0, 0]),
        ] {
            let (mut raw, b) = duplex(4096);
            let mut server = WsStream::new(b, Role::Server, BytesMut::new());
            raw.write_all(&bytes).await.unwrap();
            let mut out = [0u8; 8];
            let result = server.read(&mut out).await;
            assert!(result.is_err(), "{name}: {result:?}");
        }
    }

    async fn request(text: &str, policy: &OriginPolicy) -> (io::Result<()>, String) {
        let (mut client, server) = duplex(8192);
        let policy = policy.clone();
        let task = tokio::spawn(async move { accept(server, &policy).await.map(|_| ()) });
        // The server may stop reading (and answer) before all of a huge request is in.
        let _ = client.write_all(text.as_bytes()).await;
        let result = task.await.unwrap();
        let mut reply = vec![0u8; 2048];
        let n = client.read(&mut reply).await.unwrap_or(0);
        (result, String::from_utf8_lossy(&reply[..n]).into_owned())
    }

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    fn upgrade(extra: &str) -> String {
        format!(
            "GET / HTTP/1.1\r\nHost: desk.test:5800\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n"
        )
    }

    #[tokio::test]
    async fn the_upgrade_is_answered_and_binary_is_chosen_when_offered() {
        let (result, reply) = request(
            &upgrade("Sec-WebSocket-Protocol: chat, binary\r\n"),
            &OriginPolicy::default(),
        )
        .await;
        result.unwrap();
        assert!(reply.starts_with("HTTP/1.1 101"), "{reply}");
        assert!(
            reply.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            "{reply}"
        );
        assert!(reply.contains("Sec-WebSocket-Protocol: binary"), "{reply}");
    }

    #[tokio::test]
    async fn plain_http_and_wrong_versions_are_refused() {
        let (result, reply) = request(
            "GET / HTTP/1.1\r\nHost: x\r\n\r\n",
            &OriginPolicy::default(),
        )
        .await;
        assert!(result.is_err());
        assert!(reply.starts_with("HTTP/1.1 426"), "{reply}");
        let (result, reply) = request(
            &upgrade("").replace("Version: 13", "Version: 8"),
            &OriginPolicy::default(),
        )
        .await;
        assert!(result.is_err());
        assert!(reply.contains("Sec-WebSocket-Version: 13"), "{reply}");
        let (result, _) =
            request(&upgrade("").replace(KEY, "short"), &OriginPolicy::default()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn only_pages_from_an_allowed_origin_get_in() {
        let same = upgrade("Origin: https://desk.test:5800\r\n");
        let other = upgrade("Origin: https://evil.example\r\n");
        // Same host by default; no Origin (not a browser) passes.
        assert!(request(&same, &OriginPolicy::default()).await.0.is_ok());
        assert!(
            request(&upgrade(""), &OriginPolicy::default())
                .await
                .0
                .is_ok()
        );
        let (result, reply) = request(&other, &OriginPolicy::default()).await;
        assert!(result.is_err());
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        // Any, and an explicit list.
        assert!(request(&other, &OriginPolicy::Any).await.0.is_ok());
        let list = OriginPolicy::List(vec!["https://evil.example".into()]);
        assert!(request(&other, &list).await.0.is_ok());
        assert!(request(&same, &list).await.0.is_err());
    }

    #[tokio::test]
    async fn an_oversized_request_is_refused() {
        let junk = format!("GET / HTTP/1.1\r\nX: {}\r\n", "a".repeat(MAX_REQUEST));
        let (result, reply) = request(&junk, &OriginPolicy::default()).await;
        assert!(result.is_err());
        assert!(reply.starts_with("HTTP/1.1 431"), "{reply}");
    }
}
