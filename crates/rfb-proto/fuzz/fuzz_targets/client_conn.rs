//! Server-to-client bytes into a `ClientConn`: from scratch (handshake
//! parsers) and after a good handshake (messages, rectangles, framers).
#![no_main]

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use rfb_proto::{conn::ClientConn, handshake::ClientConfig};
use rfb_proto_fuzz::{established_client, split_chunk};

fn drive(mut conn: ClientConn, data: &[u8], chunk: usize) {
    let (mut buf, mut out) = (BytesMut::new(), Vec::new());
    for part in data.chunks(chunk) {
        buf.extend_from_slice(part);
        loop {
            match conn.poll(&mut buf, &mut out) {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    assert!(
                        conn.poll(&mut buf, &mut out).is_err(),
                        "failure must be sticky"
                    );
                    return;
                }
            }
        }
        assert!(buf.len() <= data.len());
    }
}

fuzz_target!(|data: &[u8]| {
    let (chunk, rest) = split_chunk(data);
    drive(ClientConn::new(ClientConfig::default()), rest, chunk);
    drive(established_client(), rest, chunk);
});
