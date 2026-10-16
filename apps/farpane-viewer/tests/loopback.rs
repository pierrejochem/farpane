//! `RemoteScreen` against an in-process `rfb-tokio` server.

use std::time::Duration;

use farpane_viewer::{RemoteScreen, ScreenEvent};
use rfb_codec::Damage;
use rfb_proto::{
    conn::EncodedRect,
    consts::enc,
    msg::{client::ClientMessage, server::RectHeader},
    types::Rect,
};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn rect(x: u16, y: u16) -> RectHeader {
    RectHeader {
        rect: Rect {
            x,
            y,
            width: 1,
            height: 1,
        },
        encoding: enc::RAW,
    }
}

/// A TCP proxy that hands the server's bytes to the client one at a time, so
/// the client's reads are cut at every possible point.
async fn trickle(server_addr: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let server = TcpStream::connect(server_addr).await.unwrap();
        let (mut client_r, mut client_w) = client.into_split();
        let (mut server_r, mut server_w) = server.into_split();
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut client_r, &mut server_w).await;
        });
        let mut byte = [0u8; 1];
        while server_r.read_exact(&mut byte).await.is_ok() {
            if client_w.write_all(&byte).await.is_err() {
                break;
            }
            client_w.flush().await.ok();
            tokio::task::yield_now().await;
        }
    });
    addr
}

async fn serve() -> (
    String,
    tokio::task::JoinHandle<ServerSession<tokio::net::TcpStream>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        ServerSession::accept(sock, ServerOptions::new(ServerAuth::None, 8, 4, b"loop"))
            .await
            .unwrap()
    });
    (addr, task)
}

#[tokio::test]
async fn connect_negotiates_and_input_reaches_the_server() {
    let (addr, server) = serve().await;
    let mut screen = RemoteScreen::connect(&addr, None).await.unwrap();
    let mut server = server.await.unwrap();
    assert_eq!(screen.name(), "loop");
    assert_eq!(
        (screen.framebuffer().width(), screen.framebuffer().height()),
        (8, 4)
    );

    // The viewer opens with SetPixelFormat, SetEncodings and a full request.
    assert!(matches!(
        server.next_message().await.unwrap(),
        ClientMessage::SetPixelFormat(_)
    ));
    assert!(matches!(
        server.next_message().await.unwrap(),
        ClientMessage::SetEncodings(list) if list.first() == Some(&enc::ZRLE)
    ));
    assert!(matches!(
        server.next_message().await.unwrap(),
        ClientMessage::FramebufferUpdateRequest {
            incremental: false,
            ..
        }
    ));

    screen.key(0xFF0D, true).await.unwrap();
    screen.pointer(0b001, 3, 2).await.unwrap();
    screen.cut_text(b"copy".to_vec()).await.unwrap();
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0xFF0D
        }
    );
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::PointerEvent {
            buttons: 1,
            x: 3,
            y: 2
        }
    );
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::ClientCutText(b"copy".to_vec())
    );
}

#[tokio::test]
async fn next_event_loses_nothing_when_it_is_cancelled() {
    let (server_addr, server) = serve().await;
    let addr = trickle(server_addr).await;
    let mut screen = RemoteScreen::connect(&addr, None).await.unwrap();
    let mut server = server.await.unwrap();

    // Through the proxy every byte arrives alone, so the 1 ms timeouts below
    // cancel `next_event` in the middle of messages and rectangles.
    tokio::spawn(async move {
        for _ in 0..3 {
            server.next_message().await.unwrap(); // pixel format, encodings, request
        }
        let pixels = [[9u8, 9, 9, 0], [8, 8, 8, 0], [7, 7, 7, 0]];
        let rects: Vec<EncodedRect<'_>> = pixels
            .iter()
            .enumerate()
            .map(|(i, p)| EncodedRect {
                header: rect(u16::try_from(i).unwrap(), 0),
                payload: p,
            })
            .collect();
        server.send_update(&rects).await.unwrap();
        // Keep the session open while the client reads.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut events = Vec::new();
    while events.len() < 4 {
        if let Ok(ev) = tokio::time::timeout(Duration::from_millis(1), screen.next_event()).await {
            events.push(ev.unwrap());
        }
    }
    let damaged: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ScreenEvent::Damage(Damage::Rect(r)) => Some((r.x, r.y)),
            _ => None,
        })
        .collect();
    assert_eq!(damaged, [(0, 0), (1, 0), (2, 0)]);
    assert_eq!(events[3], ScreenEvent::UpdateEnd);
    assert_eq!(screen.framebuffer().pixel(2, 0), Some([7, 7, 7, 255]));
}

#[tokio::test]
async fn cursor_shapes_arrive_as_events() {
    let (addr, server) = serve().await;
    let mut screen = RemoteScreen::connect(&addr, None).await.unwrap();
    let mut server = server.await.unwrap();
    for _ in 0..3 {
        server.next_message().await.unwrap(); // pixel format, encodings, request
    }
    // 2 x 1 cursor, hotspot (1, 0): red (opaque) and green (transparent).
    let payload = [0, 0, 255, 0, 0, 255, 0, 0, 0b1000_0000];
    server
        .send_update(&[EncodedRect {
            header: RectHeader {
                rect: Rect {
                    x: 1,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                encoding: rfb_proto::consts::pseudo::CURSOR,
            },
            payload: &payload,
        }])
        .await
        .unwrap();
    let ScreenEvent::Cursor(cursor) = screen.next_event().await.unwrap() else {
        panic!("expected a cursor event");
    };
    assert_eq!(cursor.hotspot, (1, 0));
    assert_eq!((cursor.width, cursor.height), (2, 1));
    assert_eq!(cursor.rgba, [255, 0, 0, 255, 0, 255, 0, 0]);
    assert_eq!(screen.next_event().await.unwrap(), ScreenEvent::UpdateEnd);
    // The picture was not touched.
    assert_eq!(screen.framebuffer().pixel(1, 0), Some([0, 0, 0, 255]));
}
