//! Apple Remote Desktop (type 30): the client side against scripted server
//! bytes, whole and one byte at a time.

use alloc::{vec, vec::Vec};

use super::*;
use crate::{ProtoError, consts::security};

const PRIME: [u8; 64] = [0xC3; 64];
const SERVER_KEY: [u8; 64] = [0x5A; 64];
const CREDENTIALS: [u8; 128] = [0x11; 128];
const PUBLIC_KEY: [u8; 64] = [0x77; 64];

fn config() -> ClientConfig {
    ClientConfig {
        security: vec![security::APPLE_DH],
        ..ClientConfig::default()
    }
}

/// The server's bytes up to and including the parameters.
fn server_hello(version: &[u8; 12], len: u16) -> Vec<u8> {
    let mut v = version.to_vec();
    v.extend_from_slice(&[1, security::APPLE_DH]);
    v.extend_from_slice(&2u16.to_be_bytes());
    v.extend_from_slice(&len.to_be_bytes());
    v.extend_from_slice(&PRIME[..usize::from(len)]);
    v.extend_from_slice(&SERVER_KEY[..usize::from(len)]);
    v
}

/// Feed `input` in pieces of `chunk` bytes; collect output and the event.
fn feed(
    c: &mut ClientHandshake,
    input: &[u8],
    chunk: usize,
) -> Result<(Vec<u8>, Option<ClientEvent>), ProtoError> {
    let (mut buf, mut out, mut event) = (Vec::new(), Vec::new(), None);
    for piece in input.chunks(chunk) {
        buf.extend_from_slice(piece);
        loop {
            let (n, ev) = c.poll(&buf, &mut out)?;
            buf.drain(..n);
            if let Some(e) = ev {
                event = Some(e);
            }
            if n == 0 {
                break;
            }
        }
    }
    Ok((out, event))
}

#[test]
fn the_parameters_arrive_whole_or_byte_by_byte() {
    for version in [b"RFB 003.008\n", b"RFB 003.007\n", b"RFB 003.889\n"] {
        for chunk in [usize::MAX, 1] {
            let mut c = ClientHandshake::new(config());
            let (out, event) = feed(&mut c, &server_hello(version, 64), chunk).unwrap();
            assert_eq!(
                event,
                Some(ClientEvent::AppleDhParams {
                    generator: 2,
                    prime: PRIME.to_vec(),
                    server_key: SERVER_KEY.to_vec(),
                }),
                "{version:?} chunk {chunk}"
            );
            // The version reply and the chosen type.
            assert_eq!(out.len(), 12 + 1);
            assert_eq!(*out.last().unwrap(), security::APPLE_DH);
            assert_eq!(c.security_type(), Some(security::APPLE_DH));
        }
    }
}

#[test]
fn golden_reply_bytes_then_security_result() {
    let mut c = ClientHandshake::new(config());
    feed(&mut c, &server_hello(b"RFB 003.008\n", 64), 1).unwrap();
    let mut out = Vec::new();
    c.apple_dh_reply(&CREDENTIALS, &PUBLIC_KEY, &mut out)
        .unwrap();
    let mut want = CREDENTIALS.to_vec();
    want.extend_from_slice(&PUBLIC_KEY);
    assert_eq!(out, want);
    // SecurityResult OK, then ServerInit follows (ClientInit goes out).
    let (out, _) = feed(&mut c, &0u32.to_be_bytes(), 1).unwrap();
    assert_eq!(out, [1]);
}

#[test]
fn a_reply_out_of_order_is_refused() {
    let mut c = ClientHandshake::new(config());
    let mut out = Vec::new();
    assert!(
        c.apple_dh_reply(&CREDENTIALS, &PUBLIC_KEY, &mut out)
            .is_err()
    );
    assert_eq!(out.len(), 0);
}

#[test]
fn key_lengths_are_bounded() {
    for len in [0u16, 1025, 0xFFFF] {
        let mut c = ClientHandshake::new(config());
        let mut hello = b"RFB 003.008\n".to_vec();
        hello.extend_from_slice(&[1, security::APPLE_DH]);
        hello.extend_from_slice(&2u16.to_be_bytes());
        hello.extend_from_slice(&len.to_be_bytes());
        assert!(
            matches!(
                feed(&mut c, &hello, 1),
                Err(ProtoError::LimitExceeded { .. })
            ),
            "length {len}"
        );
    }
}

#[test]
fn rfb_3_3_cannot_use_it() {
    let mut c = ClientHandshake::new(ClientConfig {
        max_version: crate::ProtocolVersion::V3_3,
        ..config()
    });
    let mut hello = b"RFB 003.003\n".to_vec();
    hello.extend_from_slice(&u32::from(security::APPLE_DH).to_be_bytes());
    assert_eq!(
        feed(&mut c, &hello, 1).unwrap_err(),
        ProtoError::NoAcceptableSecurity
    );
}
