use alloc::{vec, vec::Vec};

use super::*;
use crate::{PixelFormat, ProtoError, ProtocolVersion as V, consts::security};

const CHALLENGE: [u8; 16] = [7; 16];
const RESPONSE: [u8; 16] = [9; 16];

/// What the pair of machines observed.
#[derive(Debug, Default)]
struct Run {
    client_to_server: Vec<u8>,
    client_event: Option<ClientEvent>,
    shared: Option<bool>,
}

/// Drive both machines to completion. `chunk` limits how many bytes are
/// delivered per poll, so `chunk = 1` is the byte-by-byte split-input test.
/// `accept` decides the VNC Auth outcome.
fn run(
    ccfg: ClientConfig,
    scfg: ServerConfig,
    chunk: usize,
    accept: bool,
) -> (Result<Run, ProtoError>, ClientHandshake, ServerHandshake) {
    let mut c = ClientHandshake::new(ccfg);
    let mut to_client = Vec::new();
    let mut s = ServerHandshake::new(scfg, &mut to_client);
    let mut to_server = Vec::new();
    let (mut c_in, mut s_in) = (Vec::new(), Vec::new());
    let mut run = Run::default();
    let r = (|| {
        for _ in 0..10_000 {
            let mut progressed = false;
            // Server -> client.
            let take = to_client.len().min(chunk);
            c_in.extend(to_client.drain(..take));
            let mut out = Vec::new();
            let (n, ev) = c.poll(&c_in, &mut out)?;
            c_in.drain(..n);
            progressed |= take > 0 || n > 0 || !out.is_empty();
            run.client_to_server.extend_from_slice(&out);
            to_server.extend_from_slice(&out);
            match ev {
                Some(ClientEvent::VncAuthChallenge(ch)) => {
                    assert_eq!(ch, CHALLENGE);
                    let mut out = Vec::new();
                    c.vnc_auth_reply(&RESPONSE, &mut out)?;
                    run.client_to_server.extend_from_slice(&out);
                    to_server.extend_from_slice(&out);
                    progressed = true;
                }
                Some(e @ ClientEvent::ServerInit { .. }) => run.client_event = Some(e),
                None => {}
            }
            // Client -> server.
            let take = to_server.len().min(chunk);
            s_in.extend(to_server.drain(..take));
            let mut out = Vec::new();
            let (n, ev) = s.poll(&s_in, &mut out)?;
            s_in.drain(..n);
            progressed |= take > 0 || n > 0 || !out.is_empty();
            to_client.extend_from_slice(&out);
            let mut out = Vec::new();
            match ev {
                Some(ServerEvent::VncAuthSelected) => {
                    s.send_challenge(&CHALLENGE, &mut out)?;
                }
                Some(ServerEvent::VncAuthResponse(r)) => {
                    assert_eq!(r, RESPONSE);
                    s.security_result(accept, b"bad password", &mut out)?;
                }
                Some(ServerEvent::ClientInit { shared }) => {
                    run.shared = Some(shared);
                    s.server_init(1920, 1080, &PixelFormat::RGB888, b"desk", &mut out)?;
                }
                None => {}
            }
            progressed |= !out.is_empty();
            to_client.extend_from_slice(&out);
            if c.is_done() && s.is_done() {
                return Ok(());
            }
            if !progressed {
                break;
            }
        }
        Err(ProtoError::Malformed {
            ctx: "test stalled",
        })
    })();
    (r.map(|()| run), c, s)
}

fn cfg(max: V, sec: &[u8]) -> (ClientConfig, ServerConfig) {
    (
        ClientConfig {
            max_version: max,
            shared: true,
            security: vec![security::VNC_AUTH, security::NONE],
        },
        ServerConfig {
            max_version: max,
            security: sec.to_vec(),
        },
    )
}

fn expect_init(run: &Run) {
    assert_eq!(
        run.client_event,
        Some(ClientEvent::ServerInit {
            width: 1920,
            height: 1080,
            pixel_format: PixelFormat::RGB888,
            name: b"desk".to_vec(),
        })
    );
    assert_eq!(run.shared, Some(true));
}

#[test]
fn golden_3_8_none_client_bytes() {
    // rfbproto.rst: ProtocolVersion, Security (U8 chosen), ClientInit. With
    // None, 3.8 still sends SecurityResult (server side), 3.7 does not.
    let (c, s) = cfg(V::V3_8, &[security::NONE]);
    let (r, ..) = run(c, s, usize::MAX, true);
    let r = r.unwrap();
    // version reply, chosen security type (None), ClientInit(shared).
    let mut want = b"RFB 003.008\n".to_vec();
    want.extend_from_slice(&[1, 1]);
    assert_eq!(r.client_to_server, want);
    expect_init(&r);
}

#[test]
fn golden_3_8_vnc_auth_client_bytes() {
    // rfbproto.rst, VNC Authentication: 16-byte response, then ClientInit.
    let (c, s) = cfg(V::V3_8, &[security::VNC_AUTH]);
    let (r, ..) = run(c, s, usize::MAX, true);
    let r = r.unwrap();
    let mut want = b"RFB 003.008\n".to_vec();
    want.push(2);
    want.extend_from_slice(&RESPONSE);
    want.push(1);
    assert_eq!(r.client_to_server, want);
    expect_init(&r);
}

#[test]
fn all_versions_and_security_types_complete() {
    for v in [V::V3_3, V::V3_7, V::V3_8] {
        for sec in [security::NONE, security::VNC_AUTH] {
            for chunk in [usize::MAX, 1] {
                let (c, s) = cfg(v, &[sec]);
                let (r, cl, sv) = run(c, s, chunk, true);
                expect_init(&r.unwrap_or_else(|e| panic!("{v:?} sec {sec} chunk {chunk}: {e}")));
                assert_eq!(cl.version(), v);
                assert_eq!(sv.version(), v);
            }
        }
    }
}

#[test]
fn server_downgrades_to_old_client() {
    let (mut c, s) = cfg(V::V3_3, &[security::NONE]);
    c.max_version = V::V3_3;
    let s = ServerConfig {
        max_version: V::V3_8,
        ..s
    };
    let (r, cl, sv) = run(c, s, usize::MAX, true);
    expect_init(&r.unwrap());
    assert_eq!((cl.version(), sv.version()), (V::V3_3, V::V3_3));
}

#[test]
fn failed_auth_reports_reason_on_3_8_only() {
    let (c, s) = cfg(V::V3_8, &[security::VNC_AUTH]);
    let (r, ..) = run(c, s, 1, false);
    assert_eq!(
        r.unwrap_err(),
        ProtoError::SecurityFailed(Some("bad password".into()))
    );
    let (c, s) = cfg(V::V3_7, &[security::VNC_AUTH]);
    let (r, ..) = run(c, s, 1, false);
    assert_eq!(r.unwrap_err(), ProtoError::SecurityFailed(None));
}

#[test]
fn client_rejects_unacceptable_security() {
    let (mut c, s) = cfg(V::V3_8, &[security::VNC_AUTH]);
    c.security = vec![security::NONE];
    let (r, ..) = run(c, s, usize::MAX, true);
    assert_eq!(r.unwrap_err(), ProtoError::NoAcceptableSecurity);
}

#[test]
fn server_reason_list_zero_count_is_reported() {
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut out = Vec::new();
    let mut input = b"RFB 003.008\n".to_vec();
    input.push(0);
    input.extend_from_slice(&4u32.to_be_bytes());
    input.extend_from_slice(b"full");
    assert_eq!(
        c.poll(&input, &mut out).unwrap_err(),
        ProtoError::SecurityFailed(Some("full".into()))
    );
}

#[test]
fn rejects_non_rfb_and_oversized_input() {
    let mut c = ClientHandshake::new(ClientConfig::default());
    assert!(c.poll(b"HTTP/1.1 200", &mut Vec::new()).is_err());
    // A failed machine stays failed.
    assert!(c.poll(b"", &mut Vec::new()).is_err());

    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut out = Vec::new();
    let mut input = b"RFB 003.008\n".to_vec();
    input.extend_from_slice(&[1, 1]); // None offered, accepted by default cfg
    input.extend_from_slice(&0u32.to_be_bytes()); // SecurityResult OK
    input.extend_from_slice(&[0xFF, 0xFF, 0, 10]); // width 65535
    input.extend_from_slice(&PixelFormat::RGB888.to_bytes());
    let err = c.poll(&input, &mut out).unwrap_err();
    assert!(matches!(err, ProtoError::LimitExceeded { .. }), "{err:?}");
}

#[test]
fn server_rejects_unoffered_security_choice() {
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(ServerConfig::default(), &mut out);
    let mut input = b"RFB 003.008\n".to_vec();
    input.push(security::NONE);
    assert_eq!(
        s.poll(&input, &mut out).unwrap_err(),
        ProtoError::NoAcceptableSecurity
    );
}

#[test]
fn replies_out_of_order_are_errors() {
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(ServerConfig::default(), &mut out);
    assert!(s.send_challenge(&CHALLENGE, &mut out).is_err());
    assert!(s.security_result(true, b"", &mut out).is_err());
    let mut c = ClientHandshake::new(ClientConfig::default());
    assert!(c.vnc_auth_reply(&RESPONSE, &mut out).is_err());
}

/// Server transcripts, byte for byte, one for each version. `sec` is the
/// single security type offered.
#[test]
fn golden_server_transcripts() {
    let init_tail = {
        let mut v = vec![0x07, 0x80, 0x04, 0x38]; // 1920 x 1080
        v.extend_from_slice(&PixelFormat::RGB888.to_bytes());
        v.extend_from_slice(&[0, 0, 0, 4]);
        v.extend_from_slice(b"desk");
        v
    };
    let finish = |s: &mut ServerHandshake, input: &[u8], out: &mut Vec<u8>| {
        let (n, ev) = s.poll(input, out).unwrap();
        assert_eq!((n, ev), (1, Some(ServerEvent::ClientInit { shared: true })));
        s.server_init(1920, 1080, &PixelFormat::RGB888, b"desk", out)
            .unwrap();
        assert!(s.is_done());
    };

    // 3.8, None: version, [count, types], SecurityResult OK, ServerInit.
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(
        ServerConfig {
            max_version: V::V3_8,
            security: vec![security::NONE],
        },
        &mut out,
    );
    assert_eq!(s.poll(b"RFB 003.008\n\x01", &mut out).unwrap(), (13, None));
    finish(&mut s, &[1], &mut out);
    let mut want = b"RFB 003.008\n\x01\x01\x00\x00\x00\x00".to_vec();
    want.extend_from_slice(&init_tail);
    assert_eq!(out, want);

    // 3.7, None: no SecurityResult.
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(
        ServerConfig {
            max_version: V::V3_8,
            security: vec![security::NONE],
        },
        &mut out,
    );
    assert_eq!(s.poll(b"RFB 003.007\n\x01", &mut out).unwrap(), (13, None));
    finish(&mut s, &[1], &mut out);
    let mut want = b"RFB 003.008\n\x01\x01".to_vec();
    want.extend_from_slice(&init_tail);
    assert_eq!(out, want);

    // 3.3, None: server picks, U32 security type 1, no SecurityResult.
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(
        ServerConfig {
            max_version: V::V3_8,
            security: vec![security::NONE],
        },
        &mut out,
    );
    assert_eq!(s.poll(b"RFB 003.003\n", &mut out).unwrap(), (12, None));
    finish(&mut s, &[1], &mut out);
    let mut want = b"RFB 003.008\n\x00\x00\x00\x01".to_vec();
    want.extend_from_slice(&init_tail);
    assert_eq!(out, want);

    // 3.8, VNC Auth: challenge out, response in, SecurityResult OK.
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(ServerConfig::default(), &mut out);
    let (n, ev) = s.poll(b"RFB 003.008\n\x02", &mut out).unwrap();
    assert_eq!((n, ev), (13, Some(ServerEvent::VncAuthSelected)));
    s.send_challenge(&CHALLENGE, &mut out).unwrap();
    let (n, ev) = s.poll(&RESPONSE, &mut out).unwrap();
    assert_eq!((n, ev), (16, Some(ServerEvent::VncAuthResponse(RESPONSE))));
    s.security_result(true, b"", &mut out).unwrap();
    let mut want = b"RFB 003.008\n\x01\x02".to_vec();
    want.extend_from_slice(&CHALLENGE);
    want.extend_from_slice(&[0, 0, 0, 0]);
    assert_eq!(out, want);

    // 3.8 failure carries a reason, 3.7 does not.
    let mut out = Vec::new();
    let mut s = ServerHandshake::new(ServerConfig::default(), &mut out);
    s.poll(b"RFB 003.008\n\x02", &mut out).unwrap();
    s.send_challenge(&CHALLENGE, &mut out).unwrap();
    s.poll(&RESPONSE, &mut out).unwrap();
    out.clear();
    s.security_result(false, b"no", &mut out).unwrap();
    assert_eq!(out, [0, 0, 0, 1, 0, 0, 0, 2, b'n', b'o']);
    let mut s = ServerHandshake::new(ServerConfig::default(), &mut Vec::new());
    s.poll(b"RFB 003.007\n\x02", &mut out).unwrap();
    s.send_challenge(&CHALLENGE, &mut out).unwrap();
    s.poll(&RESPONSE, &mut out).unwrap();
    out.clear();
    s.security_result(false, b"no", &mut out).unwrap();
    assert_eq!(out, [0, 0, 0, 1]);
}

/// Client transcripts for 3.3 and 3.7, byte for byte.
#[test]
fn golden_client_transcripts_3_3_and_3_7() {
    let server_init = {
        let mut v = vec![0x07, 0x80, 0x04, 0x38];
        v.extend_from_slice(&PixelFormat::RGB888.to_bytes());
        v.extend_from_slice(&[0, 0, 0, 4]);
        v.extend_from_slice(b"desk");
        v
    };

    // 3.7, None: reply version, choose 1, ClientInit at once.
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut out = Vec::new();
    let mut input = b"RFB 003.007\n\x01\x01".to_vec();
    input.extend_from_slice(&server_init);
    let (n, ev) = c.poll(&input, &mut out).unwrap();
    assert_eq!(n, input.len());
    assert!(matches!(ev, Some(ClientEvent::ServerInit { .. })));
    assert_eq!(out, b"RFB 003.007\n\x01\x01");

    // 3.3, VNC Auth: security type is a server-chosen U32; no selection byte.
    let mut c = ClientHandshake::new(ClientConfig::default());
    let mut out = Vec::new();
    let mut input = b"RFB 003.003\n\x00\x00\x00\x02".to_vec();
    input.extend_from_slice(&CHALLENGE);
    let (n, ev) = c.poll(&input, &mut out).unwrap();
    assert_eq!(
        (n, ev),
        (input.len(), Some(ClientEvent::VncAuthChallenge(CHALLENGE)))
    );
    c.vnc_auth_reply(&RESPONSE, &mut out).unwrap();
    let mut input = vec![0, 0, 0, 0]; // SecurityResult OK
    input.extend_from_slice(&server_init);
    let (_, ev) = c.poll(&input, &mut out).unwrap();
    assert!(matches!(ev, Some(ClientEvent::ServerInit { .. })));
    let mut want = b"RFB 003.003\n".to_vec();
    want.extend_from_slice(&RESPONSE);
    want.push(1);
    assert_eq!(out, want);
}
