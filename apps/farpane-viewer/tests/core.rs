//! The connection thread against an in-process `rfb-tokio` server.

use std::time::Duration;

use farpane_viewer::core::{Command, Core, UiEvent};
use rfb_proto::{
    conn::EncodedRect,
    consts::{enc, pseudo},
    msg::{client::ClientMessage, server::RectHeader},
    types::Rect,
};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::net::{TcpListener, TcpStream};

type Session = ServerSession<TcpStream>;

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

async fn listen() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    (listener, addr)
}

async fn accept(listener: &TcpListener) -> Session {
    let (sock, _) = listener.accept().await.unwrap();
    ServerSession::accept(
        sock,
        ServerOptions::new(ServerAuth::None, 6, 4, b"core test"),
    )
    .await
    .unwrap()
}

fn connect(core: &Core, addr: &str) {
    core.send(Command::Connect {
        addr: addr.to_owned(),
        password: None,
    });
}

/// Wait for the next event, up to five seconds.
async fn next(core: &Core) -> UiEvent {
    for _ in 0..1000 {
        if let Some(e) = core.try_recv() {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no event within 5 s");
}

/// Read client messages until one matches, skipping the opening handshake
/// traffic and update requests.
async fn expect_message(server: &mut Session, want: ClientMessage) {
    loop {
        let m = server.next_message().await.unwrap();
        if m == want {
            return;
        }
        assert!(
            matches!(
                m,
                ClientMessage::SetPixelFormat(_)
                    | ClientMessage::SetEncodings(_)
                    | ClientMessage::FramebufferUpdateRequest { .. }
            ),
            "unexpected message {m:?} while waiting for {want:?}"
        );
    }
}

#[tokio::test]
async fn connect_frames_input_and_incremental_requests() {
    let (listener, addr) = listen().await;
    let core = Core::spawn(|| {});
    connect(&core, &addr);
    let mut server = accept(&listener).await;
    assert_eq!(next(&core).await, UiEvent::Connecting);
    assert_eq!(
        next(&core).await,
        UiEvent::Connected {
            name: "core test".into(),
            width: 6,
            height: 4
        }
    );

    // A 2 x 1 raw rect at (1, 2): wire pixels are B, G, R, X.
    let pixels = [10, 20, 30, 0, 40, 50, 60, 0];
    server
        .send_update(&[EncodedRect {
            header: hdr(enc::RAW, 1, 2, 2, 1),
            payload: &pixels,
        }])
        .await
        .unwrap();
    assert_eq!(
        next(&core).await,
        UiEvent::Frame {
            rect: Rect {
                x: 1,
                y: 2,
                width: 2,
                height: 1
            },
            rgba: vec![30, 20, 10, 255, 60, 50, 40, 255],
        }
    );
    // The finished update makes the viewer ask for changes only.
    loop {
        if let ClientMessage::FramebufferUpdateRequest {
            incremental: true, ..
        } = server.next_message().await.unwrap()
        {
            break;
        }
    }

    core.send(Command::Key {
        keysym: 0x61,
        down: true,
    });
    core.send(Command::Pointer {
        buttons: 1,
        x: 4,
        y: 3,
    });
    core.send(Command::CutText(b"hi".to_vec()));
    expect_message(
        &mut server,
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0x61,
        },
    )
    .await;
    expect_message(
        &mut server,
        ClientMessage::PointerEvent {
            buttons: 1,
            x: 4,
            y: 3,
        },
    )
    .await;
    expect_message(&mut server, ClientMessage::ClientCutText(b"hi".to_vec())).await;
}

#[tokio::test]
async fn resize_reports_the_new_size_then_a_full_frame() {
    let (listener, addr) = listen().await;
    let core = Core::spawn(|| {});
    connect(&core, &addr);
    let mut server = accept(&listener).await;
    next(&core).await; // Connecting
    next(&core).await; // Connected
    // A server only learns what the client accepts from its SetEncodings.
    for _ in 0..3 {
        server.next_message().await.unwrap(); // pixel format, encodings, request
    }
    server
        .send_update(&[EncodedRect {
            header: hdr(pseudo::DESKTOP_SIZE, 0, 0, 3, 2),
            payload: &[],
        }])
        .await
        .unwrap();
    assert_eq!(
        next(&core).await,
        UiEvent::Resized {
            width: 3,
            height: 2
        }
    );
    match next(&core).await {
        UiEvent::Frame { rect, rgba } => {
            assert_eq!(
                rect,
                Rect {
                    x: 0,
                    y: 0,
                    width: 3,
                    height: 2
                }
            );
            assert_eq!(rgba.len(), 3 * 2 * 4);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn disconnect_then_connect_again() {
    let (listener, addr) = listen().await;
    let core = Core::spawn(|| {});
    for round in 0..2 {
        connect(&core, &addr);
        let _server = accept(&listener).await;
        assert_eq!(next(&core).await, UiEvent::Connecting, "round {round}");
        assert!(matches!(next(&core).await, UiEvent::Connected { .. }));
        core.send(Command::Disconnect);
        assert_eq!(next(&core).await, UiEvent::Disconnected(None));
    }
}

#[tokio::test]
async fn failures_are_reported_with_a_reason() {
    // Nothing listens: connection refused.
    let (listener, addr) = listen().await;
    drop(listener);
    let core = Core::spawn(|| {});
    connect(&core, &addr);
    assert_eq!(next(&core).await, UiEvent::Connecting);
    assert!(matches!(next(&core).await, UiEvent::Disconnected(Some(_))));

    // The server hangs up mid-session.
    let (listener, addr) = listen().await;
    connect(&core, &addr);
    let server = accept(&listener).await;
    next(&core).await;
    next(&core).await;
    drop(server);
    // A clean close or a TCP reset, depending on unread data: either way the
    // user gets a reason.
    match next(&core).await {
        UiEvent::Disconnected(Some(reason)) => assert_ne!(reason, ""),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn disconnect_cancels_a_stalled_connect() {
    // The server accepts the socket and says nothing, so the handshake stalls.
    let (listener, addr) = listen().await;
    let core = Core::spawn(|| {});
    connect(&core, &addr);
    let (_silent, _) = listener.accept().await.unwrap();
    assert_eq!(next(&core).await, UiEvent::Connecting);
    core.send(Command::Disconnect);
    assert_eq!(next(&core).await, UiEvent::Disconnected(None));
}

#[tokio::test]
async fn dropping_the_handle_stops_the_thread_even_while_connected() {
    let (listener, addr) = listen().await;
    let core = Core::spawn(|| {});
    connect(&core, &addr);
    let _server = accept(&listener).await;
    next(&core).await;
    next(&core).await;
    let dropped = tokio::task::spawn_blocking(move || drop(core));
    tokio::time::timeout(Duration::from_secs(5), dropped)
        .await
        .expect("drop must not hang")
        .unwrap();
}
