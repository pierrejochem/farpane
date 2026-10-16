//! Shared setup for the fuzz targets.

use bytes::BytesMut;
use rfb_proto::{
    PixelFormat,
    conn::{ClientConn, ServerConn},
    handshake::{ClientConfig, ServerConfig},
};

/// A `ClientConn` that finished a 3.8 None handshake (1024 x 768, 32 bpp).
#[must_use]
pub fn established_client() -> ClientConn {
    let mut conn = ClientConn::new(ClientConfig::default());
    let mut buf = BytesMut::new();
    buf.extend_from_slice(b"RFB 003.008\n");
    buf.extend_from_slice(&[1, 1]); // one security type: None
    buf.extend_from_slice(&0u32.to_be_bytes()); // SecurityResult OK
    buf.extend_from_slice(&1024u16.to_be_bytes());
    buf.extend_from_slice(&768u16.to_be_bytes());
    buf.extend_from_slice(&PixelFormat::RGB888.to_bytes());
    buf.extend_from_slice(&1u32.to_be_bytes());
    buf.extend_from_slice(b"f");
    let mut out = Vec::new();
    while conn
        .poll(&mut buf, &mut out)
        .expect("valid handshake")
        .is_some()
    {}
    assert!(conn.is_established());
    conn
}

/// A `ServerConn` that finished a 3.8 None handshake (1024 x 768, 32 bpp).
#[must_use]
pub fn established_server() -> ServerConn {
    let mut out = Vec::new();
    let mut conn = ServerConn::new(
        ServerConfig {
            security: vec![rfb_proto::consts::security::NONE],
            ..ServerConfig::default()
        },
        &mut out,
    );
    let mut buf = BytesMut::new();
    buf.extend_from_slice(b"RFB 003.008\n");
    buf.extend_from_slice(&[1]); // chose None
    buf.extend_from_slice(&[1]); // ClientInit, shared
    while conn
        .poll(&mut buf, &mut out)
        .expect("valid handshake")
        .is_some()
    {}
    conn.server_init(1024, 768, &PixelFormat::RGB888, b"f", &mut out)
        .expect("server_init");
    assert!(conn.is_established());
    conn
}

/// Split a fuzz input into a chunk size (1..=64, from the first byte) and the
/// rest, so inputs also exercise partial reads.
#[must_use]
pub fn split_chunk(data: &[u8]) -> (usize, &[u8]) {
    match data.split_first() {
        Some((&c, rest)) => (usize::from(c % 64) + 1, rest),
        None => (1, data),
    }
}
