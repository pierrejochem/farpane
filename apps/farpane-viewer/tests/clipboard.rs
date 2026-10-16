//! UTF-8 clipboard through Extended Clipboard, against an in-process server.

use std::time::Duration;

use farpane_viewer::{ClipboardSent, RemoteScreen, ScreenEvent};
use rfb_codec::clipboard::{ExtClipboard, Role, flags, request_text};
use rfb_proto::{
    consts::pseudo,
    msg::{client::ClientMessage, server::ServerMessage},
};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};

type Server = ServerSession<TcpStream>;

async fn connect() -> (RemoteScreen, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        ServerSession::accept(sock, ServerOptions::new(ServerAuth::None, 8, 4, b"clip"))
            .await
            .unwrap()
    });
    let screen = RemoteScreen::connect(&addr, None).await.unwrap();
    (screen, task.await.unwrap())
}

/// Read the viewer's opening messages and return its encoding list.
async fn opening(server: &mut Server) -> Vec<i32> {
    assert!(matches!(
        server.next_message().await.unwrap(),
        ClientMessage::SetPixelFormat(_)
    ));
    let ClientMessage::SetEncodings(list) = server.next_message().await.unwrap() else {
        panic!("expected SetEncodings");
    };
    assert!(matches!(
        server.next_message().await.unwrap(),
        ClientMessage::FramebufferUpdateRequest { .. }
    ));
    list
}

/// Let the viewer process what the server sent; it has nothing to report.
async fn pump(screen: &mut RemoteScreen) {
    assert!(
        timeout(Duration::from_millis(300), screen.next_event())
            .await
            .is_err()
    );
}

async fn send(server: &mut Server, (flags, payload): (u32, Vec<u8>)) {
    server
        .send(&ServerMessage::ExtendedClipboard { flags, payload })
        .await
        .unwrap();
}

/// The next clipboard message from the viewer, handed to the server's machine.
async fn receive(server: &mut Server, clip: &mut ExtClipboard) -> rfb_codec::clipboard::Reaction {
    let ClientMessage::ExtendedClipboard { flags, payload } = server.next_message().await.unwrap()
    else {
        panic!("expected an Extended Clipboard message");
    };
    clip.on_message(flags, &payload).unwrap()
}

#[tokio::test]
async fn the_viewer_asks_for_extended_clipboard() {
    let (_screen, mut server) = connect().await;
    assert!(opening(&mut server).await.contains(&pseudo::EXT_CLIPBOARD));
}

#[tokio::test]
async fn utf8_text_travels_both_ways_without_losing_characters() {
    let (mut screen, mut server) = connect().await;
    opening(&mut server).await;
    let mut clip = ExtClipboard::new(Role::Server);

    // The server announces itself; the viewer answers with its own caps.
    send(&mut server, clip.announce()).await;
    pump(&mut screen).await;
    assert_eq!(receive(&mut server, &mut clip).await.send, []);
    assert!(clip.is_active());

    // Viewer to server: notify, request, provide.
    let text = "5\u{20ac} \u{4f60}\u{597d} \u{1f600}\nline two";
    assert_eq!(
        screen.send_clipboard(text).await.unwrap(),
        ClipboardSent::Utf8
    );
    let reply = receive(&mut server, &mut clip).await;
    assert_eq!(reply.send, [request_text()]);
    for message in reply.send {
        send(&mut server, message).await; // the server asks the viewer for the text
    }
    pump(&mut screen).await;
    let provided = receive(&mut server, &mut clip).await;
    assert_eq!(
        provided.received.as_deref(),
        Some("5\u{20ac} \u{4f60}\u{597d} \u{1f600}\r\nline two")
    );

    // Server to viewer.
    for message in clip.local_changed("\u{fc}ber\r\ncaf\u{e9} \u{65e5}\u{672c}") {
        send(&mut server, message).await;
    }
    pump(&mut screen).await;
    let ask = receive(&mut server, &mut clip).await;
    for message in ask.send {
        send(&mut server, message).await;
    }
    let event = timeout(Duration::from_secs(2), screen.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        event,
        ScreenEvent::Text("\u{fc}ber\r\ncaf\u{e9} \u{65e5}\u{672c}".into())
    );
}

#[tokio::test]
async fn without_the_extension_the_viewer_falls_back_to_latin1() {
    let (mut screen, mut server) = connect().await;
    opening(&mut server).await;
    let sent = screen
        .send_clipboard("5\u{20ac} \u{4f60}\u{597d}")
        .await
        .unwrap();
    assert_eq!(sent, ClipboardSent::Latin1 { replaced: 3 });
    assert_eq!(
        server.next_message().await.unwrap(),
        ClientMessage::ClientCutText(b"5? ??".to_vec())
    );
    // Latin-1 text still arrives the old way.
    server
        .send(&ServerMessage::ServerCutText(b"caf\xe9".to_vec()))
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(2), screen.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event, ScreenEvent::CutText(b"caf\xe9".to_vec()));
}

#[tokio::test]
async fn text_beyond_the_extended_limit_is_refused_and_nothing_is_sent() {
    let (mut screen, mut server) = connect().await;
    opening(&mut server).await;
    let mut clip = ExtClipboard::new(Role::Server);
    send(&mut server, clip.announce()).await;
    pump(&mut screen).await;
    receive(&mut server, &mut clip).await;
    let huge = "x".repeat(16 * 1024 * 1024 + 1);
    assert_eq!(
        screen.send_clipboard(&huge).await.unwrap(),
        ClipboardSent::TooLarge {
            limit: 16 * 1024 * 1024
        }
    );
    // The next small message proves nothing else was queued before it.
    screen.key(0x61, true).await.unwrap();
    assert!(matches!(
        server.next_message().await.unwrap(),
        ClientMessage::KeyEvent { .. }
    ));
}

#[tokio::test]
async fn a_damaged_extended_message_ends_the_connection() {
    let (mut screen, mut server) = connect().await;
    opening(&mut server).await;
    // Provide with a payload that is not zlib data.
    send(
        &mut server,
        (flags::PROVIDE | flags::TEXT, b"garbage".to_vec()),
    )
    .await;
    let err = timeout(Duration::from_secs(2), screen.next_event())
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("clipboard"), "{err}");
}
