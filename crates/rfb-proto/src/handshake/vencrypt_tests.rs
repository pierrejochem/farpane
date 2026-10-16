//! `VeNCrypt` 0.2 negotiation (rfbproto.rst, section on `VeNCrypt`).

use alloc::{vec, vec::Vec};

use bytes::BytesMut;

use super::*;
use crate::{
    PixelFormat, ProtoError, ProtocolVersion as V,
    conn::{ClientConn, ClientConnEvent},
    consts::{security, vencrypt},
};

const X509_NONE: u32 = vencrypt::X509_NONE;
const X509_VNC: u32 = vencrypt::X509_VNC;
const X509_PLAIN: u32 = vencrypt::X509_PLAIN;

/// What the server says after the client picked security type 19, offering
/// `subtypes`.
fn server_vencrypt_prefix(subtypes: &[u32]) -> Vec<u8> {
    let mut v = vec![0, 2, 0]; // version 0.2, version ack OK
    v.push(u8::try_from(subtypes.len()).unwrap());
    for t in subtypes {
        v.extend_from_slice(&t.to_be_bytes());
    }
    v
}

fn server_init_bytes() -> Vec<u8> {
    let mut v = vec![0x07, 0x80, 0x04, 0x38];
    v.extend_from_slice(&PixelFormat::RGB888.to_bytes());
    v.extend_from_slice(&[0, 0, 0, 4]);
    v.extend_from_slice(b"desk");
    v
}

/// Drive a client handshake with `input` in `chunk`-byte pieces, collecting
/// events and output.
fn drive(c: &mut ClientHandshake, input: &[u8], chunk: usize) -> (Vec<ClientEvent>, Vec<u8>) {
    let (mut events, mut out) = (Vec::new(), Vec::new());
    let mut buf = Vec::new();
    // An empty input still polls once (the machine may have work without bytes).
    let parts: Vec<&[u8]> = if input.is_empty() {
        vec![&[]]
    } else {
        input.chunks(chunk).collect()
    };
    for part in parts {
        buf.extend_from_slice(part);
        loop {
            let (n, ev) = c.poll(&buf, &mut out).unwrap();
            buf.drain(..n);
            match ev {
                Some(e) => events.push(e),
                None if n == 0 => break,
                None => {}
            }
            // Stop at points that need the caller to act.
            if matches!(
                events.last(),
                Some(
                    ClientEvent::StartTls { .. }
                        | ClientEvent::PlainCredentialsNeeded
                        | ClientEvent::VncAuthChallenge(_)
                )
            ) {
                return (events, out);
            }
        }
    }
    (events, out)
}

#[test]
fn golden_x509_none_transcript_3_8() {
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, security::VENCRYPT]); // one type: VeNCrypt
    input.extend(server_vencrypt_prefix(&[X509_VNC, X509_NONE]));
    input.push(1); // subtype ack: 1 = success
    let (events, out) = drive(&mut c, &input, usize::MAX);
    assert_eq!(events, [ClientEvent::StartTls { subtype: X509_NONE }]);
    // version reply, chosen type 19, VeNCrypt 0.2, the chosen subtype as U32.
    let mut want = b"RFB 003.008\n".to_vec();
    want.extend_from_slice(&[security::VENCRYPT, 0, 2]);
    want.extend_from_slice(&X509_NONE.to_be_bytes());
    assert_eq!(out, want);
    assert_eq!(c.vencrypt_subtype(), Some(X509_NONE));

    // After TLS: SecurityResult follows (even though this is "None"), then ServerInit.
    c.tls_established().unwrap();
    let mut rest = vec![0, 0, 0, 0];
    rest.extend(server_init_bytes());
    let (events, out) = drive(&mut c, &rest, usize::MAX);
    assert!(matches!(
        events.as_slice(),
        [ClientEvent::ServerInit { width: 1920, .. }]
    ));
    assert_eq!(out, [1], "ClientInit follows the SecurityResult");
    assert!(c.is_done());
}

#[test]
fn x509_vnc_continues_with_the_challenge_after_tls() {
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, security::VENCRYPT]);
    input.extend(server_vencrypt_prefix(&[X509_VNC]));
    input.push(1);
    let (events, _) = drive(&mut c, &input, 1);
    assert_eq!(events, [ClientEvent::StartTls { subtype: X509_VNC }]);
    c.tls_established().unwrap();
    let (events, _) = drive(&mut c, &[7; 16], 1);
    assert_eq!(events, [ClientEvent::VncAuthChallenge([7; 16])]);
    let mut out = Vec::new();
    c.vnc_auth_reply(&[9; 16], &mut out).unwrap();
    assert_eq!(out, [9; 16]);
}

#[test]
fn x509_plain_sends_credentials_in_the_spec_layout() {
    let mut c = ClientHandshake::new(ClientConfig {
        vencrypt: vec![X509_PLAIN],
        ..ClientConfig::default()
    });
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, security::VENCRYPT]);
    input.extend(server_vencrypt_prefix(&[X509_NONE, X509_PLAIN]));
    input.push(1);
    let (events, _) = drive(&mut c, &input, usize::MAX);
    assert_eq!(
        events,
        [ClientEvent::StartTls {
            subtype: X509_PLAIN
        }]
    );
    c.tls_established().unwrap();
    let (events, _) = drive(&mut c, &[], usize::MAX);
    assert_eq!(events, [ClientEvent::PlainCredentialsNeeded]);
    // Asked once, not on every poll.
    assert_eq!(drive(&mut c, &[], usize::MAX).0, []);

    let mut out = Vec::new();
    c.plain_credentials(b"alice", b"s3cret", &mut out).unwrap();
    let mut want = vec![0, 0, 0, 5, 0, 0, 0, 6];
    want.extend_from_slice(b"alices3cret");
    assert_eq!(
        out, want,
        "U32 username length, U32 password length, then both"
    );
    // Out of order or too long is refused.
    assert!(c.plain_credentials(b"a", b"b", &mut Vec::new()).is_err());
}

#[test]
fn overlong_credentials_are_refused() {
    let mut c = ClientHandshake::new(ClientConfig {
        vencrypt: vec![X509_PLAIN],
        ..ClientConfig::default()
    });
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, security::VENCRYPT]);
    input.extend(server_vencrypt_prefix(&[X509_PLAIN]));
    input.push(1);
    drive(&mut c, &input, usize::MAX);
    c.tls_established().unwrap();
    drive(&mut c, &[], usize::MAX);
    let long = vec![b'x'; crate::consts::limits::MAX_CREDENTIAL_LEN as usize + 1];
    assert!(matches!(
        c.plain_credentials(&long, b"", &mut Vec::new()),
        Err(ProtoError::LimitExceeded { .. })
    ));
}

#[test]
fn the_whole_negotiation_survives_byte_by_byte_delivery() {
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, security::VENCRYPT]);
    input.extend(server_vencrypt_prefix(&[X509_NONE]));
    input.push(1);
    let (events, _) = drive(&mut c, &input, 1);
    assert_eq!(events, [ClientEvent::StartTls { subtype: X509_NONE }]);
}

#[test]
fn only_certificate_validated_subtypes_are_ever_chosen() {
    // Offered: Plain (no TLS), TLSNone, TLSVnc, TLSPlain (anonymous TLS).
    // Even a client configured to accept them must not pick any.
    let mut c = ClientHandshake::new(ClientConfig {
        vencrypt: vec![
            vencrypt::PLAIN,
            vencrypt::TLS_NONE,
            vencrypt::TLS_VNC,
            vencrypt::TLS_PLAIN,
        ],
        ..ClientConfig::default()
    });
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, security::VENCRYPT]);
    input.extend(server_vencrypt_prefix(&[
        vencrypt::PLAIN,
        vencrypt::TLS_NONE,
        vencrypt::TLS_VNC,
        vencrypt::TLS_PLAIN,
    ]));
    let mut out = Vec::new();
    let r = c.poll(&input, &mut out);
    assert_eq!(r, Err(ProtoError::NoAcceptableSecurity));
    // And the preference order of acceptable ones is the caller's.
    let mut c = ClientHandshake::new(ClientConfig {
        vencrypt: vec![X509_PLAIN, X509_VNC],
        ..ClientConfig::default()
    });
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, security::VENCRYPT]);
    input.extend(server_vencrypt_prefix(&[X509_NONE, X509_VNC, X509_PLAIN]));
    input.push(1);
    assert_eq!(
        drive(&mut c, &input, usize::MAX).0,
        [ClientEvent::StartTls {
            subtype: X509_PLAIN
        }]
    );
}

#[test]
fn failures_in_the_negotiation_are_reported() {
    let go = |tail: &[u8], cfg: ClientConfig| {
        let mut c = ClientHandshake::new(cfg);
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, security::VENCRYPT]);
        input.extend_from_slice(tail);
        c.poll(&input, &mut Vec::new()).map(|(_, e)| e)
    };
    // Server only speaks VeNCrypt 0.1.
    assert!(
        matches!(go(&[0, 1], ClientConfig::default()), Err(ProtoError::SecurityFailed(Some(m))) if m.contains("older than 0.2"))
    );
    // Version ack non-zero.
    assert!(
        matches!(go(&[0, 2, 1], ClientConfig::default()), Err(ProtoError::SecurityFailed(Some(m))) if m.contains("version"))
    );
    // Subtype ack must be 1; 0 and any other value fail.
    for bad in [0u8, 2, 255] {
        let mut tail = server_vencrypt_prefix(&[X509_NONE]);
        tail.push(bad);
        assert!(
            matches!(go(&tail, ClientConfig::default()), Err(ProtoError::SecurityFailed(Some(m))) if m.contains("subtype")),
            "ack {bad}"
        );
    }
    // An empty subtype list leaves nothing to pick.
    assert_eq!(
        go(&server_vencrypt_prefix(&[]), ClientConfig::default()),
        Err(ProtoError::NoAcceptableSecurity)
    );
}

#[test]
fn vencrypt_is_preferred_when_offered_and_not_needed_when_it_is_not() {
    // Offered alongside None and VNC Auth: the client takes VeNCrypt.
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut out = Vec::new();
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[3, security::NONE, security::VNC_AUTH, security::VENCRYPT]);
    c.poll(&input, &mut out).unwrap();
    assert_eq!(out.last(), Some(&security::VENCRYPT));
    // Not offered: the old choice still works.
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut out = Vec::new();
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[2, security::NONE, security::VNC_AUTH]);
    c.poll(&input, &mut out).unwrap();
    assert_eq!(out.last(), Some(&security::VNC_AUTH));
    // 3.3 servers choose themselves and can never choose VeNCrypt.
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut input = b"RFB 003.003\n".to_vec();
    input.extend_from_slice(&u32::from(security::VENCRYPT).to_be_bytes());
    assert_eq!(
        c.poll(&input, &mut Vec::new()),
        Err(ProtoError::NoAcceptableSecurity)
    );
}

#[test]
fn tls_calls_out_of_order_are_errors() {
    let mut c = ClientHandshake::new(ClientConfig::default());
    assert!(c.tls_established().is_err());
    assert!(c.plain_credentials(b"", b"", &mut Vec::new()).is_err());
    let mut s = ServerHandshake::new(ServerConfig::default(), &mut Vec::new());
    assert!(s.tls_established(&mut Vec::new()).is_err());
}

// --- both machines against each other ---------------------------------

/// Run a client and a server through `VeNCrypt`, simulating the TLS upgrade
/// (no bytes flow for it). Returns the credentials the server received.
type Credentials = (Vec<u8>, Vec<u8>);

fn loopback(subtype: u32, version: V, accept: bool) -> Result<Option<Credentials>, ProtoError> {
    let mut to_client = Vec::new();
    let mut s = ServerHandshake::new(
        ServerConfig {
            max_version: version,
            security: vec![security::VENCRYPT],
            vencrypt: vec![subtype],
        },
        &mut to_client,
    );
    let mut c = ClientHandshake::new(ClientConfig {
        max_version: version,
        vencrypt: vec![subtype],
        ..ClientConfig::default()
    });
    let (mut to_server, mut got) = (Vec::new(), None);
    let (mut c_done, mut s_done) = (false, false);
    for _ in 0..40 {
        let mut out = Vec::new();
        let (n, ev) = c.poll(&to_client, &mut out)?;
        to_client.drain(..n);
        to_server.extend_from_slice(&out);
        match ev {
            Some(ClientEvent::StartTls { .. }) => c.tls_established()?,
            Some(ClientEvent::PlainCredentialsNeeded) => {
                let mut o = Vec::new();
                c.plain_credentials(b"alice", b"s3cret", &mut o)?;
                to_server.extend_from_slice(&o);
            }
            Some(ClientEvent::VncAuthChallenge(ch)) => {
                let mut o = Vec::new();
                c.vnc_auth_reply(&ch, &mut o)?;
                to_server.extend_from_slice(&o);
            }
            Some(ClientEvent::ServerInit { .. }) => c_done = true,
            Some(ClientEvent::StartRa2 { .. }) => panic!("no RSA-AES here"),
            None => {}
        }
        let mut out = Vec::new();
        let (n, ev) = s.poll(&to_server, &mut out)?;
        to_server.drain(..n);
        to_client.extend_from_slice(&out);
        let mut out = Vec::new();
        match ev {
            Some(ServerEvent::StartTls { .. }) => s.tls_established(&mut out)?,
            Some(ServerEvent::PlainCredentials { username, password }) => {
                got = Some((username, password));
                s.security_result(accept, b"no such user", &mut out)?;
            }
            Some(ServerEvent::VncAuthSelected) => s.send_challenge(&[5; 16], &mut out)?,
            Some(ServerEvent::VncAuthResponse(r)) => {
                assert_eq!(r, [5; 16]);
                s.security_result(accept, b"bad password", &mut out)?;
            }
            Some(ServerEvent::ClientInit { .. }) => {
                s.server_init(8, 8, &PixelFormat::RGB888, b"x", &mut out)?;
                s_done = true;
            }
            Some(ServerEvent::Ra2Selected { .. }) => panic!("no RSA-AES here"),
            None => {}
        }
        to_client.extend_from_slice(&out);
        if c_done && s_done {
            return Ok(got);
        }
    }
    Err(ProtoError::Malformed { ctx: "stalled" })
}

#[test]
fn client_and_server_agree_on_every_subtype_and_version() {
    for version in [V::V3_7, V::V3_8] {
        assert_eq!(loopback(X509_NONE, version, true), Ok(None), "{version:?}");
        assert_eq!(loopback(X509_VNC, version, true), Ok(None), "{version:?}");
        assert_eq!(
            loopback(X509_PLAIN, version, true),
            Ok(Some((b"alice".to_vec(), b"s3cret".to_vec()))),
            "{version:?}"
        );
    }
}

#[test]
fn rejected_credentials_fail_the_client_with_the_servers_reason_on_3_8() {
    assert_eq!(
        loopback(X509_PLAIN, V::V3_8, false),
        Err(ProtoError::SecurityFailed(Some("no such user".into())))
    );
    assert_eq!(
        loopback(X509_VNC, V::V3_7, false),
        Err(ProtoError::SecurityFailed(None))
    );
}

#[test]
fn the_server_refuses_what_it_did_not_offer() {
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(
        ServerConfig {
            security: vec![security::VENCRYPT],
            vencrypt: vec![X509_VNC],
            ..ServerConfig::default()
        },
        &mut out,
    );
    let mut input = b"RFB 003.008\n".to_vec();
    input.push(security::VENCRYPT);
    input.extend_from_slice(&[0, 2]);
    // The client asks for Plain (no TLS at all).
    input.extend_from_slice(&vencrypt::PLAIN.to_be_bytes());
    out.clear();
    assert_eq!(
        s.poll(&input, &mut out),
        Err(ProtoError::NoAcceptableSecurity)
    );
    assert_eq!(out.last(), Some(&0), "the client is told: not accepted");
}

#[test]
fn plain_without_tls_goes_straight_to_the_credentials() {
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(
        ServerConfig {
            security: vec![security::VENCRYPT],
            vencrypt: vec![vencrypt::PLAIN],
            ..ServerConfig::default()
        },
        &mut out,
    );
    out.clear();
    let mut input = b"RFB 003.008\n".to_vec();
    input.push(security::VENCRYPT);
    input.extend_from_slice(&[0, 2]);
    input.extend_from_slice(&vencrypt::PLAIN.to_be_bytes());
    // user "alice" (5 bytes), password "s3cret" (6 bytes)
    input.extend_from_slice(&5u32.to_be_bytes());
    input.extend_from_slice(&6u32.to_be_bytes());
    input.extend_from_slice(b"alices3cret");
    // Fed byte by byte, as the split-input convention asks.
    let mut event = None;
    let mut buf = Vec::new();
    for b in input {
        buf.push(b);
        loop {
            let (n, ev) = s.poll(&buf, &mut out).unwrap();
            buf.drain(..n);
            if ev.is_some() {
                event = ev;
            }
            if n == 0 {
                break;
            }
        }
    }
    assert_eq!(
        event,
        Some(ServerEvent::PlainCredentials {
            username: b"alice".to_vec(),
            password: b"s3cret".to_vec()
        })
    );
    // 0 (version ok), one subtype (Plain = 256); no acknowledgement byte and
    // no TLS step after the choice.
    let tail: Vec<u8> = out[out.len() - 6..].to_vec();
    assert_eq!(tail, [0, 1, 0, 0, 1, 0]);
}

#[test]
fn a_client_version_other_than_0_2_is_refused_by_the_server() {
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(
        ServerConfig {
            security: vec![security::VENCRYPT],
            vencrypt: vec![X509_VNC],
            ..ServerConfig::default()
        },
        &mut out,
    );
    let mut input = b"RFB 003.008\n".to_vec();
    input.push(security::VENCRYPT);
    input.extend_from_slice(&[0, 1]);
    assert!(matches!(
        s.poll(&input, &mut out),
        Err(ProtoError::SecurityFailed(_))
    ));
}

// --- the connection layer ------------------------------------------------

#[test]
fn plaintext_injected_before_tls_is_refused() {
    let mut conn = ClientConn::new(ClientConfig::default());
    let mut buf = BytesMut::new();
    buf.extend_from_slice(b"RFB 003.008\n");
    buf.extend_from_slice(&[1, security::VENCRYPT]);
    buf.extend_from_slice(&server_vencrypt_prefix(&[X509_NONE]));
    buf.extend_from_slice(&[1]); // the ack that starts TLS
    buf.extend_from_slice(b"INJECTED"); // an attacker's bytes, still plaintext
    let mut out = Vec::new();
    let r = loop {
        match conn.poll(&mut buf, &mut out) {
            Ok(Some(_)) => {}
            other => break other,
        }
    };
    assert!(
        matches!(r, Err(ProtoError::Malformed { ctx }) if ctx.contains("before the encryption")),
        "{r:?}"
    );

    // Without the injection the event is delivered and the buffer is empty.
    let mut conn = ClientConn::new(ClientConfig::default());
    let mut buf = BytesMut::new();
    buf.extend_from_slice(b"RFB 003.008\n");
    buf.extend_from_slice(&[1, security::VENCRYPT]);
    buf.extend_from_slice(&server_vencrypt_prefix(&[X509_NONE]));
    buf.extend_from_slice(&[1]);
    let ev = loop {
        if let Some(e) = conn.poll(&mut buf, &mut out).unwrap() {
            break e;
        }
    };
    assert_eq!(
        ev,
        ClientConnEvent::Handshake(ClientEvent::StartTls { subtype: X509_NONE })
    );
}
