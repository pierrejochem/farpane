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

/// Number of textured (non-font) image meshes in a frame's output.
fn image_meshes(output: &egui::FullOutput) -> usize {
    output
        .shapes
        .iter()
        .filter(|s| matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default()))
        .count()
}

#[tokio::test]
async fn the_servers_cursor_replaces_the_local_one_over_the_picture() {
    let (ctx, mut app, mut server) = connected().await;
    for _ in 0..3 {
        server.next_message().await.unwrap();
    }
    let over = Pos2::new(SCREEN.x / 2.0, SCREEN.y / 2.0);
    let hover = |ctx: &Context, app: &mut ViewerApp| {
        let input = RawInput {
            screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, SCREEN)),
            events: vec![Event::PointerMoved(over)],
            focused: true,
            ..RawInput::default()
        };
        ctx.run(input, |ctx| app.ui(ctx))
    };
    // No shape yet: the normal arrow, and only the picture is drawn.
    let before = hover(&ctx, &mut app);
    assert_eq!(
        before.platform_output.cursor_icon,
        egui::CursorIcon::Default
    );
    assert_eq!(app.remote_cursor_size(), None);
    let pictures = image_meshes(&before);

    // The server sends a 4 x 4 cursor.
    let mut payload = vec![255u8; 4 * 4 * 4];
    payload.extend([0b1111_0000; 4]);
    server
        .send_update(&[EncodedRect {
            header: RectHeader {
                rect: Rect {
                    x: 1,
                    y: 1,
                    width: 4,
                    height: 4,
                },
                encoding: rfb_proto::consts::pseudo::CURSOR,
            },
            payload: &payload,
        }])
        .await
        .unwrap();
    until(&ctx, &mut app, "cursor shape", |a| {
        a.remote_cursor_size() == Some((4, 4))
    })
    .await;
    let after = hover(&ctx, &mut app);
    assert_eq!(
        after.platform_output.cursor_icon,
        egui::CursorIcon::None,
        "OS cursor hidden"
    );
    assert_eq!(image_meshes(&after), pictures + 1, "the shape is drawn");

    // An empty shape hides the cursor altogether.
    server
        .send_update(&[EncodedRect {
            header: RectHeader {
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                encoding: rfb_proto::consts::pseudo::CURSOR,
            },
            payload: &[],
        }])
        .await
        .unwrap();
    until(&ctx, &mut app, "hidden cursor", |a| {
        a.remote_cursor_size() == Some((0, 0))
    })
    .await;
    let hidden = hover(&ctx, &mut app);
    assert_eq!(hidden.platform_output.cursor_icon, egui::CursorIcon::None);
    assert_eq!(image_meshes(&hidden), pictures);
}

/// Run a frame and return what egui asked the platform to put on the clipboard.
fn copied_text(ctx: &Context, app: &mut ViewerApp, events: Vec<Event>) -> Vec<String> {
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, SCREEN)),
        events,
        focused: true,
        ..RawInput::default()
    };
    ctx.run(input, |ctx| app.ui(ctx))
        .platform_output
        .commands
        .into_iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text),
            _ => None,
        })
        .collect()
}

const CTRL_KEY: Modifiers = Modifiers {
    ctrl: true,
    command: true,
    ..Modifiers::NONE
};

#[tokio::test]
async fn paste_sends_the_text_before_ctrl_v() {
    let (ctx, mut app, mut server) = connected().await;
    // egui-winit reports Ctrl+V as a Paste event carrying the local clipboard.
    frame_with(
        &ctx,
        &mut app,
        vec![Event::Paste("caf\u{e9} \u{20ac}5\r\nok".into())],
        CTRL_KEY,
    );
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::ClientCutText(b"caf\xe9 ?5\nok".to_vec())
    );
    for (keysym, down) in [(0xFFE3, true), (0x76, true), (0x76, false)] {
        assert_eq!(
            next_input(&mut server).await,
            ClientMessage::KeyEvent { down, keysym }
        );
    }
    assert!(
        app.notice().is_some_and(|n| n.contains("Latin-1")),
        "{:?}",
        app.notice()
    );
}

#[tokio::test]
async fn copy_and_cut_reach_the_remote_as_keys() {
    let (ctx, mut app, mut server) = connected().await;
    frame_with(&ctx, &mut app, vec![Event::Copy], CTRL_KEY);
    for (keysym, down) in [(0xFFE3, true), (0x63, true), (0x63, false)] {
        assert_eq!(
            next_input(&mut server).await,
            ClientMessage::KeyEvent { down, keysym }
        );
    }
    frame_with(&ctx, &mut app, vec![Event::Cut], CTRL_KEY);
    for (keysym, down) in [(0x78, true), (0x78, false)] {
        assert_eq!(
            next_input(&mut server).await,
            ClientMessage::KeyEvent { down, keysym }
        );
    }
}

#[tokio::test]
async fn with_sharing_off_only_the_keys_go_through() {
    let (ctx, mut app, mut server) = connected().await;
    app.set_clipboard_sync(false);
    frame_with(
        &ctx,
        &mut app,
        vec![Event::Paste("secret".into())],
        CTRL_KEY,
    );
    // No ClientCutText: the first thing the server sees is the keystrokes.
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
            keysym: 0x76
        }
    );
    // And text from the server is not copied.
    server
        .send(&rfb_proto::msg::server::ServerMessage::ServerCutText(
            b"remote".to_vec(),
        ))
        .await
        .unwrap();
    for _ in 0..30 {
        assert_eq!(copied_text(&ctx, &mut app, vec![]), Vec::<String>::new());
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn server_text_lands_on_the_local_clipboard_without_control_characters() {
    let (ctx, mut app, mut server) = connected().await;
    server
        .send(&rfb_proto::msg::server::ServerMessage::ServerCutText(
            b"rm -rf \x1b[2J\x07 caf\xe9\r\nline2".to_vec(),
        ))
        .await
        .unwrap();
    for _ in 0..600 {
        let copied = copied_text(&ctx, &mut app, vec![]);
        if let Some(text) = copied.first() {
            assert_eq!(text, "rm -rf [2J caf\u{e9}\nline2");
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server's text never reached the clipboard");
}

#[tokio::test]
async fn oversized_clipboard_text_is_not_sent_and_the_session_survives() {
    let (ctx, mut app, mut server) = connected().await;
    let big = "x".repeat(1024 * 1024 + 1);
    frame_with(&ctx, &mut app, vec![Event::Paste(big)], CTRL_KEY);
    assert!(
        app.notice().is_some_and(|n| n.contains("too large")),
        "{:?}",
        app.notice()
    );
    // No ClientCutText came through, only the shortcut keys, and the
    // connection is still usable afterwards.
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
            keysym: 0x76
        }
    );
    frame(&ctx, &mut app, vec![Event::Text("z".into())]);
    loop {
        match next_input(&mut server).await {
            ClientMessage::KeyEvent {
                keysym: 0x7A,
                down: true,
            } => break,
            ClientMessage::ClientCutText(_) => panic!("oversized text was sent"),
            _ => {}
        }
    }
}

/// Run a frame at egui time `time` (seconds) with the pointer at `pointer`.
fn frame_at(
    ctx: &Context,
    app: &mut ViewerApp,
    time: f64,
    pointer: Option<Pos2>,
    events: Vec<Event>,
) -> egui::FullOutput {
    let mut events = events;
    if let Some(p) = pointer {
        events.push(Event::PointerMoved(p));
    } else {
        events.push(Event::PointerGone);
    }
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(time),
        events,
        focused: true,
        ..RawInput::default()
    };
    ctx.run(input, |ctx| app.ui(ctx))
}

fn fullscreen_commands(output: &egui::FullOutput) -> Vec<bool> {
    output.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::ViewportCommand::Fullscreen(on) => Some(*on),
            _ => None,
        })
        .collect()
}

fn f11(pressed: bool) -> Event {
    key(Key::F11, pressed, Modifiers::NONE)
}

#[tokio::test]
async fn f11_toggles_full_screen_and_is_not_sent_to_the_remote() {
    let (ctx, mut app, mut server) = connected().await;
    assert!(!app.is_fullscreen());
    let out = frame_at(&ctx, &mut app, 10.0, None, vec![f11(true), f11(false)]);
    assert_eq!(fullscreen_commands(&out), [true]);
    assert!(app.is_fullscreen());
    let out = frame_at(&ctx, &mut app, 11.0, None, vec![f11(true), f11(false)]);
    assert_eq!(fullscreen_commands(&out), [false]);
    assert!(!app.is_fullscreen());
    // F11 never reached the server: the next input it sees is the 'a'.
    frame(&ctx, &mut app, vec![Event::Text("a".into())]);
    assert_eq!(
        next_input(&mut server).await,
        ClientMessage::KeyEvent {
            down: true,
            keysym: 0x61
        }
    );
}

#[tokio::test]
async fn the_toolbar_opens_at_the_top_edge_and_closes_after_two_seconds() {
    let (ctx, mut app, _server) = connected().await;
    frame_at(&ctx, &mut app, 100.0, None, vec![f11(true)]);
    assert!(app.is_fullscreen());
    assert!(!app.toolbar_open(), "closed when full screen starts");

    // The pointer in the middle of the screen does nothing.
    frame_at(&ctx, &mut app, 100.5, Some(Pos2::new(500.0, 300.0)), vec![]);
    assert!(!app.toolbar_open());
    // The top edge opens it.
    frame_at(&ctx, &mut app, 101.0, Some(Pos2::new(500.0, 1.0)), vec![]);
    assert!(app.toolbar_open());
    // Staying open for the next two seconds, even with the pointer elsewhere...
    frame_at(&ctx, &mut app, 102.5, Some(Pos2::new(500.0, 400.0)), vec![]);
    assert!(app.toolbar_open());
    // ...and gone after that.
    frame_at(&ctx, &mut app, 103.5, Some(Pos2::new(500.0, 400.0)), vec![]);
    assert!(!app.toolbar_open());
}

#[tokio::test]
async fn a_pinned_toolbar_stays_open() {
    let (ctx, mut app, _server) = connected().await;
    frame_at(&ctx, &mut app, 200.0, None, vec![f11(true)]);
    frame_at(&ctx, &mut app, 200.5, Some(Pos2::new(500.0, 1.0)), vec![]);
    assert!(app.toolbar_open());
    app.set_toolbar_pinned(true);
    frame_at(&ctx, &mut app, 260.0, Some(Pos2::new(500.0, 400.0)), vec![]);
    assert!(app.toolbar_open(), "pinned: no auto-hide");
    app.set_toolbar_pinned(false);
    frame_at(&ctx, &mut app, 261.0, Some(Pos2::new(500.0, 400.0)), vec![]);
    frame_at(&ctx, &mut app, 264.0, Some(Pos2::new(500.0, 400.0)), vec![]);
    assert!(!app.toolbar_open(), "unpinned: hides again");
}

#[tokio::test]
async fn the_toolbar_does_not_exist_outside_full_screen() {
    let (ctx, mut app, _server) = connected().await;
    // At the top edge in a normal window: nothing opens.
    frame_at(&ctx, &mut app, 300.0, Some(Pos2::new(500.0, 1.0)), vec![]);
    assert!(!app.toolbar_open());
}

#[tokio::test]
async fn ctrl_alt_del_is_sent_as_one_sequence() {
    let (_ctx, app, mut server) = connected().await;
    app.send_ctrl_alt_del();
    for (keysym, down) in [
        (0xFFE3, true),
        (0xFFE9, true),
        (0xFFFF, true),
        (0xFFFF, false),
        (0xFFE9, false),
        (0xFFE3, false),
    ] {
        assert_eq!(
            next_input(&mut server).await,
            ClientMessage::KeyEvent { down, keysym }
        );
    }
}

#[tokio::test]
async fn a_lost_connection_leaves_full_screen() {
    let (ctx, mut app, server) = connected().await;
    frame_at(&ctx, &mut app, 400.0, None, vec![f11(true)]);
    assert!(app.is_fullscreen());
    drop(server);
    let mut left = false;
    for i in 0..600 {
        let out = frame_at(&ctx, &mut app, 401.0 + f64::from(i) * 0.01, None, vec![]);
        if fullscreen_commands(&out).contains(&false) {
            left = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        left,
        "the window should leave full screen when the server goes away"
    );
    assert!(!app.is_fullscreen());
    assert!(!app.is_connected());
}

#[tokio::test]
async fn clicks_on_the_toolbar_do_not_reach_the_remote_machine() {
    let (ctx, mut app, mut server) = connected().await;
    frame_at(&ctx, &mut app, 500.0, None, vec![f11(true)]);
    app.set_toolbar_pinned(true);
    // Open the pill and let it finish sliding.
    for i in 0..10 {
        frame_at(
            &ctx,
            &mut app,
            500.5 + f64::from(i) * 0.1,
            Some(Pos2::new(500.0, 1.0)),
            vec![],
        );
    }
    assert!(app.toolbar_open());
    // The pill is centred near the top; click on its left part (the title).
    let on_pill = Pos2::new(SCREEN.x / 2.0 - 150.0, 26.0);
    let click = |pressed| Event::PointerButton {
        pos: on_pill,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame_at(&ctx, &mut app, 502.0, Some(on_pill), vec![click(true)]);
    frame_at(&ctx, &mut app, 502.1, Some(on_pill), vec![click(false)]);
    // Now click on the picture itself: that one must arrive.
    let on_picture = Pos2::new(SCREEN.x / 2.0, SCREEN.y / 2.0);
    let pic_click = |pressed| Event::PointerButton {
        pos: on_picture,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame_at(
        &ctx,
        &mut app,
        503.0,
        Some(on_picture),
        vec![pic_click(true)],
    );
    frame_at(
        &ctx,
        &mut app,
        503.1,
        Some(on_picture),
        vec![pic_click(false)],
    );
    // The first button event the server sees is the one on the picture.
    loop {
        match next_input(&mut server).await {
            ClientMessage::PointerEvent { buttons: 1, x, y } => {
                assert!(
                    (350..=450).contains(&x) && (150..=250).contains(&y),
                    "({x}, {y})"
                );
                break;
            }
            // Plain moves (the opening hover at the top edge, the approach to
            // the picture) are fine, and so is the effect of the toolbar
            // button the click on the pill hit (Ctrl+Alt+Del); only pointer
            // clicks must not leak.
            ClientMessage::PointerEvent { buttons: 0, .. } | ClientMessage::KeyEvent { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// All text drawn in a frame, joined by newlines.
fn drawn_text(output: &egui::FullOutput) -> String {
    fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &output.shapes {
        collect(&clipped.shape, &mut out);
    }
    out.join("\n")
}

const HOTKEY: Modifiers = Modifiers {
    ctrl: true,
    alt: true,
    shift: true,
    ..Modifiers::NONE
};

#[tokio::test]
async fn the_statistics_overlay_shows_live_readings_when_switched_on() {
    let (ctx, mut app, _server) = connected().await;
    // Off by default: nothing about statistics is drawn.
    let out = frame_at(&ctx, &mut app, 1.0, None, vec![]);
    assert!(!drawn_text(&out).contains(" fps"), "{}", drawn_text(&out));

    app.set_stats_visible(true);
    // The connection thread produces a reading every quarter second.
    until(&ctx, &mut app, "a reading", |a| {
        a.stats().is_some() && a.history_len() >= 2
    })
    .await;
    let out = frame_at(&ctx, &mut app, 2.0, None, vec![]);
    let text = drawn_text(&out);
    assert!(text.contains(" fps"), "{text}");
    assert!(text.contains("800×400"), "the remote size is shown: {text}");
    assert!(text.contains("received") && text.contains("sent"), "{text}");
    let s = app.stats().unwrap();
    assert!(s.total_rx > 0 && s.total_tx > 0, "{s:?}");

    app.set_stats_visible(false);
    let out = frame_at(&ctx, &mut app, 3.0, None, vec![]);
    assert!(!drawn_text(&out).contains(" fps"));
}

#[tokio::test]
async fn the_hotkey_toggles_the_overlay_and_stays_local() {
    let (ctx, mut app, mut server) = connected().await;
    assert!(!app.stats_visible());
    let chord = |pressed| key(Key::S, pressed, HOTKEY);
    frame_with(&ctx, &mut app, vec![chord(true), chord(false)], HOTKEY);
    assert!(app.stats_visible());
    frame_with(&ctx, &mut app, vec![chord(true), chord(false)], HOTKEY);
    assert!(!app.stats_visible());
    // The remote machine never saw an 's' (0x73); the modifiers it may have.
    frame(&ctx, &mut app, vec![Event::Text("q".into())]);
    loop {
        match next_input(&mut server).await {
            ClientMessage::KeyEvent {
                keysym: 0x71,
                down: true,
            } => break,
            ClientMessage::KeyEvent {
                keysym: 0x73 | 0x53,
                ..
            } => panic!("the hotkey leaked to the remote"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn readings_are_dropped_when_the_connection_ends() {
    let (ctx, mut app, server) = connected().await;
    until(&ctx, &mut app, "a reading", |a| a.stats().is_some()).await;
    drop(server);
    until(&ctx, &mut app, "disconnect", |a| !a.is_connected()).await;
    assert!(app.stats().is_none());
    assert_eq!(app.history_len(), 0);
}
