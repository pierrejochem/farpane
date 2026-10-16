//! The window logic driven with synthetic egui input (no display needed).

use std::time::Duration;

use egui::{Context, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect as UiRect, Vec2};
use farpane_viewer::app::{Startup, ViewerApp};
use rfb_proto::{
    conn::EncodedRect,
    msg::{client::ClientMessage, server::RectHeader},
    types::Rect,
};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::net::{TcpListener, TcpStream};

const SCREEN: Vec2 = Vec2::new(1100.0, 720.0);

/// Run one egui frame with `events`.
fn frame(ctx: &Context, app: &mut ViewerApp, events: Vec<Event>) {
    frame_with(ctx, app, events, Modifiers::NONE);
}

/// Like [`frame`], with the modifiers egui-winit would report for the frame.
fn frame_with(ctx: &Context, app: &mut ViewerApp, events: Vec<Event>, modifiers: Modifiers) {
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, SCREEN)),
        events,
        modifiers,
        focused: true,
        ..RawInput::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

/// Wait for a condition while running frames.
async fn until(ctx: &Context, app: &mut ViewerApp, what: &str, cond: impl Fn(&ViewerApp) -> bool) {
    for _ in 0..600 {
        frame(ctx, app, vec![]);
        if cond(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn connected() -> (Context, ViewerApp, ServerSession<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        ServerSession::accept(
            sock,
            ServerOptions::new(ServerAuth::None, 800, 400, b"ui test"),
        )
        .await
        .unwrap()
    });
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(
        &ctx,
        Startup {
            addr: Some(addr),
            ..Startup::default()
        },
    );
    let server = server.await.unwrap();
    until(&ctx, &mut app, "connection", ViewerApp::is_connected).await;
    // Let the canvas take keyboard focus (it asks on its first frame).
    for _ in 0..3 {
        frame(&ctx, &mut app, vec![]);
    }
    (ctx, app, server)
}

fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}

/// The next client message that is not part of the opening handshake traffic.
async fn next_input(server: &mut ServerSession<TcpStream>) -> ClientMessage {
    loop {
        let m = server.next_message().await.unwrap();
        if !matches!(
            m,
            ClientMessage::SetPixelFormat(_)
                | ClientMessage::SetEncodings(_)
                | ClientMessage::FramebufferUpdateRequest { .. }
        ) {
            return m;
        }
    }
}

#[tokio::test]
async fn typing_reaches_the_server_once_connected() {
    let (ctx, mut app, mut server) = connected().await;
    assert_eq!(app.remote_size(), (800, 400));
    frame(&ctx, &mut app, vec![Event::Text("h".into())]);
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0x68
        }
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::KeyEvent {
            down: false,
            keysym: 0x68
        }
    );

    // Enter is a key event, not text; Tab and arrows must reach the remote
    // machine instead of moving egui's focus.
    let none = Modifiers::NONE;
    frame(
        &ctx,
        &mut app,
        vec![key(Key::Enter, true, none), key(Key::Enter, false, none)],
    );
    frame(
        &ctx,
        &mut app,
        vec![key(Key::Tab, true, none), key(Key::ArrowDown, true, none)],
    );
    for (keysym, down) in [
        (0xFF0D, true),
        (0xFF0D, false),
        (0xFF09, true),
        (0xFF54, true),
    ] {
        assert_eq!(
            next_input(&mut server).await,
            ClientMessage::KeyEvent { down, keysym }
        );
    }
}

#[tokio::test]
async fn mouse_maps_into_the_remote_screen_and_the_wheel_scrolls() {
    let (ctx, mut app, mut server) = connected().await;
    let centre = Pos2::new(SCREEN.x / 2.0, SCREEN.y / 2.0 + 15.0);
    frame(
        &ctx,
        &mut app,
        vec![
            Event::PointerMoved(centre),
            Event::PointerButton {
                pos: centre,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos: centre,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    // The picture is letterboxed in the window; its middle is the remote middle.
    let ClientMessage::PointerEvent { buttons: 0, x, y } = next_input(&mut server).await else {
        panic!("expected a plain move first");
    };
    assert!(
        (390..=410).contains(&x) && (190..=210).contains(&y),
        "({x}, {y})"
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::PointerEvent { buttons: 1, x, y }
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::PointerEvent { buttons: 0, x, y }
    );

    frame(
        &ctx,
        &mut app,
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, 1.0),
            modifiers: Modifiers::NONE,
        }],
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::PointerEvent { buttons: 8, x, y }
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::PointerEvent { buttons: 0, x, y }
    );
}

#[tokio::test]
async fn losing_window_focus_releases_held_keys() {
    let (ctx, mut app, mut server) = connected().await;
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    frame_with(&ctx, &mut app, vec![key(Key::C, true, ctrl)], ctrl);
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0xFFE3
        }
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0x63
        }
    );
    // The window loses focus while both are down.
    let _ = ctx.run(
        RawInput {
            screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, SCREEN)),
            focused: false,
            events: vec![Event::WindowFocused(false)],
            ..RawInput::default()
        },
        |ctx| app.ui(ctx),
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::KeyEvent {
            down: false,
            keysym: 0x63
        }
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::KeyEvent {
            down: false,
            keysym: 0xFFE3
        }
    );
}

#[tokio::test]
async fn server_pixels_and_resizes_update_the_window() {
    let (ctx, mut app, mut server) = connected().await;
    for _ in 0..3 {
        server.next_message().await.unwrap(); // opening handshake traffic
    }
    let header = RectHeader {
        rect: Rect {
            x: 0,
            y: 0,
            width: 400,
            height: 300,
        },
        encoding: farpane_viewer::DEFAULT_ENCODINGS[farpane_viewer::DEFAULT_ENCODINGS.len() - 1],
    };
    assert_eq!(header.encoding, rfb_proto::consts::pseudo::DESKTOP_SIZE);
    server
        .send_update(&[EncodedRect {
            header,
            payload: &[],
        }])
        .await
        .unwrap();
    until(&ctx, &mut app, "resize", |a| a.remote_size() == (400, 300)).await;
}
