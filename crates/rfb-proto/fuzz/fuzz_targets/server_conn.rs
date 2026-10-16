//! Client-to-server bytes into a `ServerConn`: from scratch and after a good
//! handshake.
#![no_main]

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use rfb_proto::{conn::ServerConn, handshake::ServerConfig};
use rfb_proto_fuzz::{established_server, split_chunk};

fn drive(mut conn: ServerConn, data: &[u8], chunk: usize) {
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
    let mut greeting = Vec::new();
    drive(
        ServerConn::new(ServerConfig::default(), &mut greeting),
        rest,
        chunk,
    );
    drive(established_server(), rest, chunk);
});
