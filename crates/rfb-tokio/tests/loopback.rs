//! Client and server sessions against each other over in-memory and TCP streams.

use std::time::Duration;

use bytes::Bytes;
use rfb_proto::{
    PixelFormat,
    conn::{ClientConnEvent, EncodedRect},
    consts::{enc, pseudo},
    msg::{
        client::ClientMessage,
        server::{RectHeader, ServerMessage},
    },
    types::Rect,
};
use rfb_tokio::{ClientOptions, ClientSession, Error, ServerAuth, ServerOptions, ServerSession};
use tokio::io::{AsyncWriteExt, duplex};

fn server_opts(auth: ServerAuth) -> ServerOptions {
    ServerOptions::new(auth, 100, 50, b"test desktop")
}

fn client_opts(password: Option<&[u8]>) -> ClientOptions {
    ClientOptions {
        password: password.map(<[u8]>::to_vec),
        ..ClientOptions::default()
    }
}

fn rect(encoding: i32, x: u16, y: u16, w: u16, h: u16) -> RectHeader {
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

#[tokio::test]
async fn none_and_vnc_auth_handshakes_complete() {
    for (auth, password) in [
        (ServerAuth::None, None),
        (
            ServerAuth::VncPassword(b"hunter2".to_vec()),
            Some(&b"hunter2"[..]),
        ),
    ] {
        // A tiny pipe forces every read and write to be partial.
        let (c, s) = duplex(7);
        let server = tokio::spawn(ServerSession::accept(s, server_opts(auth)));
        let (client, info) = ClientSession::connect(c, client_opts(password))
            .await
            .unwrap();
        let server = server.await.unwrap().unwrap();
        assert_eq!((info.width, info.height), (100, 50));
        assert_eq!(info.name, b"test desktop");
        assert_eq!(info.pixel_format, PixelFormat::RGB888);
        assert!(server.shared());
        assert_eq!(client.conn().framebuffer_size(), (100, 50));
    }
}

#[tokio::test]
async fn wrong_password_fails_on_both_sides() {
    let (c, s) = duplex(64);
    let server = tokio::spawn(ServerSession::accept(
        s,
        server_opts(ServerAuth::VncPassword(b"right".to_vec())),
    ));
    let err = ClientSession::connect(c, client_opts(Some(b"wrong")))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Proto(e) if e.to_string().contains("authentication failed")),
        "{err}"
    );
    assert!(matches!(
        server.await.unwrap().unwrap_err(),
        Error::AuthFailed
    ));
}

#[tokio::test]
async fn missing_password_is_reported() {
    let (c, s) = duplex(64);
    tokio::spawn(ServerSession::accept(
        s,
        server_opts(ServerAuth::VncPassword(b"pw".to_vec())),
    ));
    assert!(matches!(
        ClientSession::connect(c, client_opts(None))
            .await
            .unwrap_err(),
        Error::PasswordRequired
    ));
}

#[tokio::test]
async fn messages_and_updates_flow_both_ways() {
    let (c, s) = duplex(4096);
    let server = tokio::spawn(ServerSession::accept(s, server_opts(ServerAuth::None)));
    let (mut client, _) = ClientSession::connect(c, client_opts(None)).await.unwrap();
    let mut server = server.await.unwrap().unwrap();

    let msgs = [
        ClientMessage::SetEncodings(vec![enc::COPY_RECT, pseudo::DESKTOP_SIZE]),
        ClientMessage::FramebufferUpdateRequest {
            incremental: false,
            rect: Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 50,
            },
        },
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0x41,
        },
        ClientMessage::PointerEvent {
            buttons: 1,
            x: 3,
            y: 4,
        },
    ];
    for m in &msgs {
        client.send(m).await.unwrap();
    }
    for m in &msgs {
        assert_eq!(&server.next_message().await.unwrap(), m);
    }
    assert_eq!(
        server.conn().client_encodings(),
        [enc::COPY_RECT, pseudo::DESKTOP_SIZE]
    );

    let raw = vec![0x5A; 6 * 4 * 4];
    server
        .send_update(&[
            EncodedRect {
                header: rect(enc::RAW, 1, 2, 6, 4),
                payload: &raw,
            },
            EncodedRect {
                header: rect(enc::COPY_RECT, 10, 10, 8, 8),
                payload: &[0, 1, 0, 2],
            },
        ])
        .await
        .unwrap();
    server.send(&ServerMessage::Bell).await.unwrap();

    assert_eq!(
        client.next_event().await.unwrap(),
        ClientConnEvent::UpdateBegin { rects: 2 }
    );
    assert_eq!(
        client.next_event().await.unwrap(),
        ClientConnEvent::Rect {
            header: rect(enc::RAW, 1, 2, 6, 4),
            payload: Bytes::from(raw),
        }
    );
    assert!(matches!(
        client.next_event().await.unwrap(),
        ClientConnEvent::Rect { .. }
    ));
    assert_eq!(
        client.next_event().await.unwrap(),
        ClientConnEvent::UpdateEnd
    );
    assert_eq!(client.next_event().await.unwrap(), ClientConnEvent::Bell);
}

#[tokio::test]
async fn invalid_updates_are_refused_before_anything_is_written() {
    let (c, s) = duplex(64);
    let server = tokio::spawn(ServerSession::accept(s, server_opts(ServerAuth::None)));
    let (_client, _) = ClientSession::connect(c, client_opts(None)).await.unwrap();
    let mut server = server.await.unwrap().unwrap();
    let err = server
        .send_update(&[EncodedRect {
            header: rect(enc::RAW, 99, 0, 5, 5),
            payload: &[0; 100],
        }])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Proto(_)), "{err}");
}

#[tokio::test]
async fn messages_pipelined_right_after_client_init_are_not_lost() {
    // A raw client that sends ClientInit and a message in one write.
    let (mut c, s) = duplex(4096);
    let server = tokio::spawn(ServerSession::accept(s, server_opts(ServerAuth::None)));
    let mut wire = b"RFB 003.008\n".to_vec();
    wire.push(1); // choose None
    wire.push(1); // ClientInit: shared
    ClientMessage::KeyEvent {
        down: true,
        keysym: 7,
    }
    .write(&mut wire)
    .unwrap();
    c.write_all(&wire).await.unwrap();
    let mut server = server.await.unwrap().unwrap();
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::KeyEvent {
            down: true,
            keysym: 7
        }
    );
}

#[tokio::test]
async fn hangup_is_reported_as_closed() {
    let (c, s) = duplex(64);
    let server = tokio::spawn(ServerSession::accept(s, server_opts(ServerAuth::None)));
    let (mut client, _) = ClientSession::connect(c, client_opts(None)).await.unwrap();
    drop(server.await.unwrap().unwrap());
    assert!(matches!(
        client.next_event().await.unwrap_err(),
        Error::Closed
    ));
}

#[tokio::test(start_paused = true)]
async fn silent_peers_time_out() {
    // Server never answers.
    let (c, _s) = duplex(64);
    let opts = ClientOptions {
        handshake_timeout: Duration::from_secs(5),
        ..ClientOptions::default()
    };
    assert!(matches!(
        ClientSession::connect(c, opts).await.unwrap_err(),
        Error::HandshakeTimeout
    ));
    // Client never answers.
    let (_c, s) = duplex(64);
    let mut opts = server_opts(ServerAuth::None);
    opts.handshake_timeout = Duration::from_secs(5);
    assert!(matches!(
        ServerSession::accept(s, opts).await.unwrap_err(),
        Error::HandshakeTimeout
    ));
}

#[tokio::test]
async fn works_over_tcp() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        ServerSession::accept(sock, server_opts(ServerAuth::VncPassword(b"pw".to_vec())))
            .await
            .unwrap()
    });
    let sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut client, info) = ClientSession::connect(sock, client_opts(Some(b"pw")))
        .await
        .unwrap();
    assert_eq!(info.width, 100);
    let mut server = server.await.unwrap();
    client
        .send(&ClientMessage::ClientCutText(b"over tcp".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::ClientCutText(b"over tcp".to_vec())
    );
}
