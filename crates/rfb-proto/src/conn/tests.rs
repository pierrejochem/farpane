//! `ClientConn` against `ServerConn`.

use alloc::{vec, vec::Vec};

use bytes::{Bytes, BytesMut};

use super::*;
use crate::{
    PixelFormat, ProtoError, ProtocolVersion as V,
    consts::{enc, pseudo, security},
    handshake::{ClientConfig, ClientEvent, ServerConfig, ServerEvent},
    msg::{
        client::ClientMessage,
        server::{RectHeader, ServerMessage},
    },
    types::Rect,
};

const W: u16 = 100;
const H: u16 = 50;
const CHALLENGE: [u8; 16] = [3; 16];
const RESPONSE: [u8; 16] = [5; 16];

struct Pair {
    client: ClientConn,
    server: ServerConn,
    to_client: BytesMut,
    to_server: BytesMut,
}

impl Pair {
    fn new(v: V, sec: u8) -> Self {
        let mut to_client = Vec::new();
        let server = ServerConn::new(
            ServerConfig {
                max_version: v,
                security: vec![sec],
            },
            &mut to_client,
        );
        let client = ClientConn::new(ClientConfig {
            max_version: v,
            ..ClientConfig::default()
        });
        Self {
            client,
            server,
            to_client: BytesMut::from(&to_client[..]),
            to_server: BytesMut::new(),
        }
    }

    /// Run until both sides are established, answering handshake events.
    fn handshake(&mut self) {
        for _ in 0..32 {
            let mut c_out = Vec::new();
            while let Some(ev) = self.client.poll(&mut self.to_client, &mut c_out).unwrap() {
                if let ClientConnEvent::Handshake(ClientEvent::VncAuthChallenge(c)) = ev {
                    assert_eq!(c, CHALLENGE);
                    self.client.vnc_auth_reply(&RESPONSE, &mut c_out).unwrap();
                }
            }
            self.to_server.extend_from_slice(&c_out);
            let mut s_out = Vec::new();
            while let Some(ev) = self.server.poll(&mut self.to_server, &mut s_out).unwrap() {
                match ev {
                    ServerConnEvent::Handshake(ServerEvent::VncAuthSelected) => {
                        self.server.send_challenge(&CHALLENGE, &mut s_out).unwrap();
                    }
                    ServerConnEvent::Handshake(ServerEvent::VncAuthResponse(r)) => {
                        assert_eq!(r, RESPONSE);
                        self.server.security_result(true, b"", &mut s_out).unwrap();
                    }
                    ServerConnEvent::Handshake(ServerEvent::ClientInit { shared }) => {
                        assert!(shared);
                        self.server
                            .server_init(W, H, &PixelFormat::RGB888, b"srv", &mut s_out)
                            .unwrap();
                    }
                    other @ ServerConnEvent::Message(_) => panic!("{other:?}"),
                }
            }
            self.to_client.extend_from_slice(&s_out);
            if self.client.is_established() && self.server.is_established() {
                return;
            }
        }
        panic!("handshake stalled");
    }

    fn client_sends(&mut self, msg: &ClientMessage) {
        let mut out = Vec::new();
        self.client.send(msg, &mut out).unwrap();
        self.to_server.extend_from_slice(&out);
    }

    fn server_events(&mut self) -> Vec<ServerConnEvent> {
        let mut out = Vec::new();
        let mut events = Vec::new();
        while let Some(ev) = self.server.poll(&mut self.to_server, &mut out).unwrap() {
            events.push(ev);
        }
        events
    }

    fn client_events(&mut self) -> Vec<ClientConnEvent> {
        let mut out = Vec::new();
        let mut events = Vec::new();
        while let Some(ev) = self.client.poll(&mut self.to_client, &mut out).unwrap() {
            events.push(ev);
        }
        events
    }
}

fn hdr(encoding: i32, x: u16, y: u16, w: u16, h: u16) -> RectHeader {
    RectHeader {
        rect: Rect {
            x,
            y,
            width: w,
            height: h,
        },
        encoding,
    }
}

#[test]
fn every_version_and_security_type_reaches_the_message_loop() {
    for v in [V::V3_3, V::V3_7, V::V3_8] {
        for sec in [security::NONE, security::VNC_AUTH] {
            let mut p = Pair::new(v, sec);
            p.handshake();
            assert_eq!(p.client.version(), v);
            assert_eq!(p.server.version(), v);
            assert_eq!(p.client.framebuffer_size(), (W, H));
            assert_eq!(p.server.framebuffer_size(), (W, H));
        }
    }
}

#[test]
fn client_messages_reach_the_server_and_update_its_state() {
    let mut p = Pair::new(V::V3_8, security::NONE);
    p.handshake();
    let pf16 = PixelFormat {
        bits_per_pixel: 16,
        depth: 16,
        ..PixelFormat::RGB888
    };
    let msgs = vec![
        ClientMessage::SetPixelFormat(pf16),
        ClientMessage::SetEncodings(vec![enc::HEXTILE, enc::COPY_RECT, pseudo::DESKTOP_SIZE]),
        ClientMessage::FramebufferUpdateRequest {
            incremental: false,
            rect: Rect {
                x: 0,
                y: 0,
                width: W,
                height: H,
            },
        },
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0x61,
        },
        ClientMessage::PointerEvent {
            buttons: 1,
            x: 5,
            y: 6,
        },
        ClientMessage::ClientCutText(b"clip".to_vec()),
    ];
    for m in &msgs {
        p.client_sends(m);
    }
    let got: Vec<_> = p
        .server_events()
        .into_iter()
        .map(|e| match e {
            ServerConnEvent::Message(m) => m,
            other @ ServerConnEvent::Handshake(_) => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(got, msgs);
    assert_eq!(p.server.client_pixel_format(), &pf16);
    assert_eq!(
        p.server.client_encodings(),
        [enc::HEXTILE, enc::COPY_RECT, pseudo::DESKTOP_SIZE]
    );
}

#[test]
fn server_update_arrives_at_the_client_in_the_clients_format() {
    let mut p = Pair::new(V::V3_8, security::NONE);
    p.handshake();
    let pf16 = PixelFormat {
        bits_per_pixel: 16,
        depth: 16,
        ..PixelFormat::RGB888
    };
    p.client_sends(&ClientMessage::SetPixelFormat(pf16));
    p.client_sends(&ClientMessage::SetEncodings(vec![enc::COPY_RECT]));
    p.server_events();

    let raw = [9u8; 4 * 2 * 2];
    let rects = [
        EncodedRect {
            header: hdr(enc::RAW, 10, 10, 4, 2),
            payload: &raw,
        },
        EncodedRect {
            header: hdr(enc::COPY_RECT, 0, 0, 5, 5),
            payload: &[0, 1, 0, 2],
        },
    ];
    let mut out = Vec::new();
    p.server.send_update(&rects, &mut out).unwrap();
    p.server.send(&ServerMessage::Bell, &mut out).unwrap();
    p.to_client.extend_from_slice(&out);
    assert_eq!(
        p.client_events(),
        vec![
            ClientConnEvent::UpdateBegin { rects: 2 },
            ClientConnEvent::Rect {
                header: hdr(enc::RAW, 10, 10, 4, 2),
                payload: Bytes::copy_from_slice(&raw),
            },
            ClientConnEvent::Rect {
                header: hdr(enc::COPY_RECT, 0, 0, 5, 5),
                payload: Bytes::from_static(&[0, 1, 0, 2]),
            },
            ClientConnEvent::UpdateEnd,
            ClientConnEvent::Bell,
        ]
    );
}

#[test]
fn desktop_size_moves_the_bounds_on_both_sides() {
    let mut p = Pair::new(V::V3_8, security::NONE);
    p.handshake();
    p.client_sends(&ClientMessage::SetEncodings(vec![
        pseudo::DESKTOP_SIZE,
        enc::COPY_RECT,
    ]));
    p.server_events();
    let rects = [
        EncodedRect {
            header: hdr(pseudo::DESKTOP_SIZE, 0, 0, 300, 200),
            payload: &[],
        },
        EncodedRect {
            header: hdr(enc::COPY_RECT, 250, 150, 10, 10),
            payload: &[0; 4],
        },
    ];
    let mut out = Vec::new();
    p.server.send_update(&rects, &mut out).unwrap();
    p.to_client.extend_from_slice(&out);
    assert_eq!(p.client_events().len(), 4);
    assert_eq!(p.server.framebuffer_size(), (300, 200));
    assert_eq!(p.client.framebuffer_size(), (300, 200));
}

#[test]
fn send_update_refuses_bad_rects_and_writes_nothing() {
    let mut p = Pair::new(V::V3_8, security::NONE);
    p.handshake();
    p.client_sends(&ClientMessage::SetEncodings(vec![enc::COPY_RECT]));
    p.server_events();
    let try_send = |p: &mut Pair, header, payload: &[u8]| {
        let mut out = Vec::new();
        let r = p
            .server
            .send_update(&[EncodedRect { header, payload }], &mut out);
        if r.is_err() {
            assert_eq!(out.len(), 0, "nothing written on error");
        }
        r
    };
    // Not requested.
    assert_eq!(
        try_send(&mut p, hdr(enc::HEXTILE, 0, 0, 1, 1), &[0, 0, 0, 0, 0]),
        Err(ProtoError::EncodingNotRequested(enc::HEXTILE))
    );
    // DesktopSize and Cursor need to be asked for too.
    assert_eq!(
        try_send(&mut p, hdr(pseudo::DESKTOP_SIZE, 0, 0, 5, 5), &[]),
        Err(ProtoError::EncodingNotRequested(pseudo::DESKTOP_SIZE))
    );
    // Outside the framebuffer.
    assert!(matches!(
        try_send(&mut p, hdr(enc::RAW, 99, 0, 2, 1), &[0; 8]),
        Err(ProtoError::Malformed { .. })
    ));
    // Payload too short or too long for Raw.
    assert!(matches!(
        try_send(&mut p, hdr(enc::RAW, 0, 0, 2, 1), &[0; 7]),
        Err(ProtoError::Malformed { .. })
    ));
    assert!(matches!(
        try_send(&mut p, hdr(enc::RAW, 0, 0, 2, 1), &[0; 9]),
        Err(ProtoError::Malformed { .. })
    ));
    // The state survived all of that.
    assert!(try_send(&mut p, hdr(enc::RAW, 0, 0, 2, 1), &[0; 8]).is_ok());
}

#[test]
fn nothing_can_be_sent_before_the_handshake_ends() {
    let mut out = Vec::new();
    let mut s = ServerConn::new(ServerConfig::default(), &mut out);
    assert!(s.send_update(&[], &mut out).is_err());
    assert!(s.send(&ServerMessage::Bell, &mut out).is_err());
}

#[test]
fn update_begin_goes_through_send_update_only() {
    let mut p = Pair::new(V::V3_8, security::NONE);
    p.handshake();
    let mut out = Vec::new();
    assert!(
        p.server
            .send(&ServerMessage::UpdateBegin { rects: 1 }, &mut out)
            .is_err()
    );
    // An empty update is legal.
    p.server.send_update(&[], &mut out).unwrap();
    assert_eq!(out, [0, 0, 0, 0]);
}

#[test]
fn server_errors_are_fatal_and_sticky() {
    let mut p = Pair::new(V::V3_8, security::NONE);
    p.handshake();
    p.to_server.extend_from_slice(&[99]);
    let mut out = Vec::new();
    assert!(p.server.poll(&mut p.to_server, &mut out).is_err());
    assert!(!p.server.is_established());
    assert!(p.server.poll(&mut p.to_server, &mut out).is_err());
}

#[test]
fn byte_by_byte_client_stream_is_parsed_by_the_server() {
    let mut p = Pair::new(V::V3_8, security::NONE);
    p.handshake();
    let mut stream = Vec::new();
    ClientMessage::SetEncodings(vec![enc::ZRLE, enc::RAW])
        .write(&mut stream)
        .unwrap();
    ClientMessage::ClientCutText(b"x".to_vec())
        .write(&mut stream)
        .unwrap();
    let mut events = 0;
    let mut out = Vec::new();
    for b in stream {
        p.to_server.extend_from_slice(&[b]);
        while p.server.poll(&mut p.to_server, &mut out).unwrap().is_some() {
            events += 1;
        }
    }
    assert_eq!(events, 2);
}
