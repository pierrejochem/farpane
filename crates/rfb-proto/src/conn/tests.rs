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
                ..ServerConfig::default()
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
                    other => panic!("{other:?}"),
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

mod robustness {
    //! Whatever the peer sends, the connections return, stay bounded and, once
    //! failed, stay failed.

    use proptest::prelude::*;

    use super::*;
    use crate::framing;

    /// Feed `bytes` in pieces cut at `cuts`, polling after each piece.
    fn drive_client(conn: &mut ClientConn, bytes: &[u8], cuts: &[usize]) {
        let (mut buf, mut out) = (BytesMut::new(), Vec::new());
        let mut failed = false;
        let mut start = 0;
        for end in cuts
            .iter()
            .map(|c| c % (bytes.len() + 1))
            .chain([bytes.len()])
            .scan(0, |hi, c| {
                *hi = c.max(*hi);
                Some(*hi)
            })
        {
            buf.extend_from_slice(&bytes[start..end]);
            start = end;
            loop {
                match conn.poll(&mut buf, &mut out) {
                    Ok(Some(_)) => assert!(!failed, "event after failure"),
                    Ok(None) => break,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            assert!(buf.len() <= bytes.len());
            if failed {
                assert!(conn.poll(&mut buf, &mut out).is_err(), "failure is sticky");
            }
        }
    }

    fn drive_server(conn: &mut ServerConn, bytes: &[u8], cuts: &[usize]) {
        let (mut buf, mut out) = (BytesMut::new(), Vec::new());
        let mut failed = false;
        let mut start = 0;
        for end in cuts
            .iter()
            .map(|c| c % (bytes.len() + 1))
            .chain([bytes.len()])
            .scan(0, |hi, c| {
                *hi = c.max(*hi);
                Some(*hi)
            })
        {
            buf.extend_from_slice(&bytes[start..end]);
            start = end;
            loop {
                match conn.poll(&mut buf, &mut out) {
                    Ok(Some(_)) => assert!(!failed, "event after failure"),
                    Ok(None) => break,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                assert!(conn.poll(&mut buf, &mut out).is_err(), "failure is sticky");
            }
        }
    }

    fn bytes() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 0..400)
    }

    fn cuts() -> impl Strategy<Value = Vec<usize>> {
        prop::collection::vec(any::<usize>(), 0..6)
    }

    fn pixel_format() -> impl Strategy<Value = PixelFormat> {
        prop_oneof![Just(8u8), Just(16), Just(32)].prop_map(|bpp| PixelFormat {
            bits_per_pixel: bpp,
            depth: bpp.min(24),
            ..PixelFormat::RGB888
        })
    }

    proptest! {
        /// Random bytes straight into a fresh connection: the handshake parsers.
        #[test]
        fn fresh_conns_survive_garbage(input in bytes(), cuts in cuts()) {
            drive_client(&mut ClientConn::new(ClientConfig::default()), &input, &cuts);
            let mut out = Vec::new();
            drive_server(&mut ServerConn::new(ServerConfig::default(), &mut out), &input, &cuts);
        }

        /// Random bytes after a good handshake: the message and rectangle parsers.
        #[test]
        fn established_conns_survive_garbage(input in bytes(), cuts in cuts()) {
            let mut p = Pair::new(V::V3_8, security::NONE);
            p.handshake();
            drive_client(&mut p.client, &input, &cuts);
            let mut p = Pair::new(V::V3_8, security::NONE);
            p.handshake();
            drive_server(&mut p.server, &input, &cuts);
        }

        /// Garbage that starts like an update reaches the framers.
        #[test]
        fn updates_with_garbage_rects_survive(
            encoding in prop_oneof![
                Just(enc::RAW), Just(enc::COPY_RECT), Just(enc::RRE), Just(enc::CO_RRE),
                Just(enc::HEXTILE), Just(enc::ZLIB), Just(enc::ZRLE), Just(pseudo::CURSOR),
                Just(pseudo::DESKTOP_SIZE), Just(pseudo::LAST_RECT), any::<i32>(),
            ],
            rect in any::<[u16; 4]>(),
            body in bytes(),
            cuts in cuts(),
        ) {
            let mut stream = Vec::new();
            ServerMessage::UpdateBegin { rects: 1 }.write(&mut stream).unwrap();
            let header = RectHeader {
                rect: Rect { x: rect[0], y: rect[1], width: rect[2], height: rect[3] },
                encoding,
            };
            stream.extend_from_slice(&header.to_bytes());
            stream.extend_from_slice(&body);
            let mut p = Pair::new(V::V3_8, security::NONE);
            p.handshake();
            drive_client(&mut p.client, &stream, &cuts);
        }

        #[test]
        fn framers_never_overreach(
            encoding in prop_oneof![
                Just(enc::RAW), Just(enc::COPY_RECT), Just(enc::RRE), Just(enc::CO_RRE),
                Just(enc::HEXTILE), Just(enc::ZLIB), Just(enc::ZRLE), Just(pseudo::CURSOR),
                any::<i32>(),
            ],
            rect in any::<[u16; 4]>(),
            pf in pixel_format(),
            body in bytes(),
        ) {
            let header = RectHeader {
                rect: Rect { x: rect[0], y: rect[1], width: rect[2], height: rect[3] },
                encoding,
            };
            match framing::frame(&header, &pf, &body) {
                Ok(n) => prop_assert!(n <= body.len()),
                Err(ProtoError::NeedMore(n)) => prop_assert!(n > 0),
                Err(_) => {}
            }
        }

        /// Whatever `send_update` accepts, the client reads back unchanged.
        #[test]
        fn accepted_updates_round_trip(
            rects in prop::collection::vec(
                (0u16..W, 0u16..H, 0u16..8, 0u16..8, any::<u8>()), 0..6),
        ) {
            let mut p = Pair::new(V::V3_8, security::NONE);
            p.handshake();
            let payloads: Vec<Vec<u8>> = rects
                .iter()
                .map(|&(x, y, w, h, fill)| {
                    let w = w.min(W - x);
                    let h = h.min(H - y);
                    vec![fill; usize::from(w) * usize::from(h) * 4]
                })
                .collect();
            let encoded: Vec<EncodedRect<'_>> = rects
                .iter()
                .zip(&payloads)
                .map(|(&(x, y, w, h, _), payload)| EncodedRect {
                    header: hdr(enc::RAW, x, y, w.min(W - x), h.min(H - y)),
                    payload,
                })
                .collect();
            let mut out = Vec::new();
            p.server.send_update(&encoded, &mut out).unwrap();
            p.to_client.extend_from_slice(&out);
            let events = p.client_events();
            prop_assert_eq!(events.len(), encoded.len() + 2);
            for (ev, want) in events[1..].iter().zip(&encoded) {
                prop_assert_eq!(
                    ev,
                    &ClientConnEvent::Rect {
                        header: want.header,
                        payload: Bytes::copy_from_slice(want.payload),
                    }
                );
            }
        }
    }
}

mod resize {
    use super::*;
    use crate::types::Screen;

    /// An `ExtendedDesktopSize` rectangle: `reason` in x, `status` in y.
    fn ext_rect(
        reason: u16,
        status: u16,
        w: u16,
        h: u16,
        screens: &[Screen],
    ) -> (RectHeader, Vec<u8>) {
        let mut payload = vec![u8::try_from(screens.len()).unwrap(), 0, 0, 0];
        for s in screens {
            s.write(&mut payload);
        }
        (hdr(pseudo::EXT_DESKTOP_SIZE, reason, status, w, h), payload)
    }

    fn screen(id: u32, w: u16, h: u16) -> Screen {
        Screen {
            id,
            x: 0,
            y: 0,
            width: w,
            height: h,
            flags: 0,
        }
    }

    /// A connected pair whose client asked for `ExtendedDesktopSize`.
    fn pair() -> Pair {
        let mut p = Pair::new(V::V3_8, security::NONE);
        p.handshake();
        p.client_sends(&ClientMessage::SetEncodings(vec![
            enc::COPY_RECT,
            pseudo::EXT_DESKTOP_SIZE,
        ]));
        p.server_events();
        p
    }

    fn send(p: &mut Pair, rects: &[(RectHeader, Vec<u8>)]) -> Result<(), ProtoError> {
        let encoded: Vec<EncodedRect<'_>> = rects
            .iter()
            .map(|(header, payload)| EncodedRect {
                header: *header,
                payload,
            })
            .collect();
        let mut out = Vec::new();
        p.server.send_update(&encoded, &mut out)?;
        p.to_client.extend_from_slice(&out);
        Ok(())
    }

    #[test]
    fn a_successful_layout_resizes_both_sides_and_unlocks_the_request() {
        let mut p = pair();
        assert!(!p.client.supports_desktop_resize());
        // Initial layout, sent for a non-incremental request: reason 0, status 0.
        send(&mut p, &[ext_rect(0, 0, W, H, &[screen(1, W, H)])]).unwrap();
        p.client_events();
        assert!(p.client.supports_desktop_resize());

        // The server changes the size; rectangles after it use the new bounds.
        send(
            &mut p,
            &[
                ext_rect(0, 0, 640, 480, &[screen(1, 640, 480)]),
                (hdr(enc::COPY_RECT, 600, 400, 10, 10), vec![0; 4]),
            ],
        )
        .unwrap();
        assert_eq!(p.client_events().len(), 4);
        assert_eq!(p.client.framebuffer_size(), (640, 480));
        assert_eq!(p.server.framebuffer_size(), (640, 480));
    }

    #[test]
    fn a_refusal_does_not_change_the_size_even_if_its_size_fields_are_nonsense() {
        let mut p = pair();
        // Client-initiated (x = 1), "administratively prohibited" (y = 1).
        send(&mut p, &[ext_rect(1, 1, 9999, 9999, &[])]).unwrap();
        p.client_events();
        assert_eq!(p.client.framebuffer_size(), (W, H));
        assert_eq!(p.server.framebuffer_size(), (W, H));
        assert!(
            p.client.supports_desktop_resize(),
            "a refusal still proves the extension"
        );
    }

    #[test]
    fn set_desktop_size_is_refused_until_the_extension_has_been_seen() {
        let mut p = pair();
        let request = ClientMessage::SetDesktopSize {
            width: 800,
            height: 600,
            screens: vec![screen(1, 800, 600)],
        };
        let mut out = Vec::new();
        assert_eq!(
            p.client.send(&request, &mut out),
            Err(ProtoError::ExtensionNotConfirmed("SetDesktopSize"))
        );
        assert!(out.is_empty(), "nothing is written");

        send(&mut p, &[ext_rect(0, 0, W, H, &[screen(1, W, H)])]).unwrap();
        p.client_events();
        p.client_sends(&request);
        assert_eq!(
            p.server_events(),
            [ServerConnEvent::Message(request)],
            "the server receives exactly what was asked"
        );
    }

    #[test]
    fn fence_needs_the_server_to_speak_first_and_the_client_to_ask_for_it() {
        use crate::consts::fence;
        let mut p = pair(); // this client did not ask for Fence
        let ask = ServerMessage::Fence {
            flags: fence::REQUEST | fence::BLOCK_BEFORE,
            payload: vec![1, 2, 3, 4],
        };
        let mut out = Vec::new();
        assert_eq!(
            p.server.send(&ask, &mut out),
            Err(ProtoError::ExtensionNotConfirmed("Fence"))
        );
        assert_eq!(out.len(), 0);

        // A client that asked for it.
        let mut p = Pair::new(V::V3_8, security::NONE);
        p.handshake();
        p.client_sends(&ClientMessage::SetEncodings(vec![enc::RAW, pseudo::FENCE]));
        p.server_events();
        let answer = ClientMessage::Fence {
            flags: fence::BLOCK_BEFORE,
            payload: vec![1, 2, 3, 4],
        };
        let mut out = Vec::new();
        assert_eq!(
            p.client.send(&answer, &mut out),
            Err(ProtoError::ExtensionNotConfirmed("Fence")),
            "the server has not announced Fence yet"
        );
        assert!(!p.client.supports_fence());

        // The server announces and the client answers; both reach the other side.
        out.clear();
        p.server.send(&ask, &mut out).unwrap();
        p.to_client.extend_from_slice(&out);
        assert_eq!(
            p.client_events(),
            [ClientConnEvent::Fence {
                flags: fence::REQUEST | fence::BLOCK_BEFORE,
                payload: vec![1, 2, 3, 4]
            }]
        );
        assert!(p.client.supports_fence());
        p.client_sends(&answer);
        assert_eq!(p.server_events(), [ServerConnEvent::Message(answer)]);
    }

    #[test]
    fn an_oversized_layout_is_fatal() {
        let mut p = pair();
        let big = crate::consts::limits::MAX_FB_DIMENSION + 1;
        // The server side would never send it; feed the bytes directly.
        let (h, payload) = ext_rect(0, 0, big, 10, &[]);
        let mut bytes = Vec::new();
        ServerMessage::UpdateBegin { rects: 1 }
            .write(&mut bytes)
            .unwrap();
        bytes.extend_from_slice(&h.to_bytes());
        bytes.extend_from_slice(&payload);
        p.to_client.extend_from_slice(&bytes);
        let mut out = Vec::new();
        let result = loop {
            match p.client.poll(&mut p.to_client, &mut out) {
                Ok(Some(_)) => {}
                other => break other,
            }
        };
        assert!(
            matches!(result, Err(ProtoError::LimitExceeded { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn the_server_checks_that_the_layout_payload_matches_its_screen_count() {
        let mut p = pair();
        let (h, mut payload) = ext_rect(0, 0, W, H, &[screen(1, W, H)]);
        payload.pop(); // one byte short
        let r = send(&mut p, &[(h, payload)]);
        assert!(matches!(r, Err(ProtoError::Malformed { .. })), "{r:?}");
    }
}
