//! "Resize remote to window", with a server that answers `SetDesktopSize`.

use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use egui::{Context, Pos2, RawInput, Rect as UiRect, Vec2};
use farpane_viewer::app::{Startup, ViewerApp};
use rfb_proto::{
    conn::EncodedRect,
    consts::pseudo,
    msg::{client::ClientMessage, server::RectHeader},
    types::{Rect, Screen},
};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::{net::TcpListener, sync::mpsc};

#[derive(Clone, Copy)]
enum Mode {
    /// Grant exactly what was asked.
    Grant,
    /// Grant this size instead.
    GrantOther(u16, u16),
    /// Refuse with this status code.
    Refuse(u16),
}

struct Server {
    addr: String,
    mode: Arc<Mutex<Mode>>,
    requests: mpsc::UnboundedReceiver<(u16, u16)>,
}

/// A server of 64 x 48 that, if `announce`, sends its layout up front.
async fn serve(announce: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let mode = Arc::new(Mutex::new(Mode::Grant));
    let (tx, requests) = mpsc::unbounded_channel();
    let task_mode = mode.clone();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut session = ServerSession::accept(
            sock,
            ServerOptions::new(ServerAuth::None, 64, 48, b"resizable"),
        )
        .await
        .unwrap();
        // SetPixelFormat, SetEncodings, the first full request.
        for _ in 0..3 {
            session.next_message().await.unwrap();
        }
        let layout = |reason: u16, status: u16, w: u16, h: u16| {
            let screen = Screen {
                id: 5,
                x: 0,
                y: 0,
                width: w,
                height: h,
                flags: 0,
            };
            let mut payload = vec![1, 0, 0, 0];
            screen.write(&mut payload);
            (
                RectHeader {
                    rect: Rect {
                        x: reason,
                        y: status,
                        width: w,
                        height: h,
                    },
                    encoding: pseudo::EXT_DESKTOP_SIZE,
                },
                payload,
            )
        };
        if announce {
            let (header, payload) = layout(0, 0, 64, 48);
            session
                .send_update(&[EncodedRect {
                    header,
                    payload: &payload,
                }])
                .await
                .unwrap();
        }
        let mut current = (64, 48);
        while let Ok(message) = session.next_message().await {
            if let ClientMessage::SetDesktopSize {
                width,
                height,
                screens,
            } = message
            {
                let _ = tx.send((width, height));
                assert_eq!(screens.len(), 1, "a single screen is requested");
                assert_eq!(
                    screens[0].id, 5,
                    "the id of the server's screen is preserved"
                );
                assert_eq!((screens[0].width, screens[0].height), (width, height));
                let answer = match *task_mode.lock().unwrap() {
                    Mode::Grant => {
                        current = (width, height);
                        layout(1, 0, width, height)
                    }
                    Mode::GrantOther(w, h) => {
                        current = (w, h);
                        layout(1, 0, w, h)
                    }
                    Mode::Refuse(status) => layout(1, status, current.0, current.1),
                };
                let (header, payload) = answer;
                session
                    .send_update(&[EncodedRect {
                        header,
                        payload: &payload,
                    }])
                    .await
                    .unwrap();
            }
        }
    });
    Server {
        addr,
        mode,
        requests,
    }
}

/// One frame at egui time `time` with a window of `size`.
fn frame(ctx: &Context, app: &mut ViewerApp, size: Vec2, time: f64) {
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, size)),
        time: Some(time),
        focused: true,
        ..RawInput::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

/// Run frames of constant `size` from `*time` for `seconds` of egui time
/// (with a little real time so the connection thread can answer).
async fn settle(ctx: &Context, app: &mut ViewerApp, size: Vec2, time: &mut f64, seconds: f64) {
    let end = *time + seconds;
    while *time < end {
        frame(ctx, app, size, *time);
        *time += 0.1;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
}

async fn connected(server: &Server, settings: Option<&std::path::Path>) -> (Context, ViewerApp) {
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(
        &ctx,
        Startup {
            addr: Some(server.addr.clone()),
            settings_path: settings.map(std::path::Path::to_path_buf),
            ..Startup::default()
        },
    );
    for i in 0..600 {
        frame(
            &ctx,
            &mut app,
            Vec2::new(1100.0, 720.0),
            f64::from(i) * 0.01,
        );
        if app.is_connected() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(app.is_connected());
    (ctx, app)
}

const WINDOW: Vec2 = Vec2::new(1100.0, 720.0);

async fn wait_request(server: &mut Server) -> (u16, u16) {
    tokio::time::timeout(Duration::from_secs(3), server.requests.recv())
        .await
        .expect("no resize request")
        .unwrap()
}

#[tokio::test]
async fn the_remote_follows_the_window_once_it_settles_and_does_not_loop() {
    let mut server = serve(true).await;
    let (ctx, mut app) = connected(&server, None).await;
    let mut t = 100.0;
    settle(&ctx, &mut app, WINDOW, &mut t, 0.5).await;
    assert!(
        app.server_can_resize(),
        "the layout announcement was understood"
    );
    assert!(
        server.requests.try_recv().is_err(),
        "off by default: nothing asked"
    );

    app.set_resize_remote(true);
    settle(&ctx, &mut app, WINDOW, &mut t, 1.0).await;
    let (w, h) = wait_request(&mut server).await;
    assert_eq!(w, 1100, "the window's width in pixels");
    assert!(
        (580..=660).contains(&h),
        "the height between the top bar and the footer: {h}"
    );

    // The server granted it, the picture followed, and nothing more is asked.
    settle(&ctx, &mut app, WINDOW, &mut t, 2.0).await;
    assert_eq!(app.remote_size(), (w, h));
    assert!(
        server.requests.try_recv().is_err(),
        "no second request for the same size"
    );
}

#[tokio::test]
async fn a_window_that_keeps_changing_is_followed_once_at_the_end() {
    let mut server = serve(true).await;
    let (ctx, mut app) = connected(&server, None).await;
    let mut t = 100.0;
    settle(&ctx, &mut app, WINDOW, &mut t, 0.5).await;
    app.set_resize_remote(true);
    // Dragging the window edge: a new size every frame.
    for i in 0..15_u8 {
        frame(
            &ctx,
            &mut app,
            Vec2::new(900.0 + f32::from(i) * 7.0, 700.0),
            t,
        );
        t += 0.05;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    assert!(
        server.requests.try_recv().is_err(),
        "no request while the size is still changing"
    );
    let last = Vec2::new(900.0 + 14.0 * 7.0, 700.0);
    settle(&ctx, &mut app, last, &mut t, 1.0).await;
    let (w, _) = wait_request(&mut server).await;
    assert_eq!(w, 998);
    settle(&ctx, &mut app, last, &mut t, 1.0).await;
    assert!(server.requests.try_recv().is_err(), "exactly one request");
}

#[tokio::test]
async fn a_refusal_is_reported_and_stops_further_requests_until_toggled() {
    let mut server = serve(true).await;
    *server.mode.lock().unwrap() = Mode::Refuse(1);
    let (ctx, mut app) = connected(&server, None).await;
    let mut t = 100.0;
    settle(&ctx, &mut app, WINDOW, &mut t, 0.5).await;
    app.set_resize_remote(true);
    settle(&ctx, &mut app, WINDOW, &mut t, 1.0).await;
    wait_request(&mut server).await;
    settle(&ctx, &mut app, WINDOW, &mut t, 0.5).await;
    assert!(
        app.notice().is_some_and(|n| n.contains("prohibited")),
        "{:?}",
        app.notice()
    );
    assert_eq!(app.remote_size(), (64, 48), "a refusal changes nothing");

    // A different window size does not provoke another request.
    settle(&ctx, &mut app, Vec2::new(1000.0, 600.0), &mut t, 1.5).await;
    assert!(server.requests.try_recv().is_err());

    // Switching the option off and on again is a fresh try.
    *server.mode.lock().unwrap() = Mode::Grant;
    app.set_resize_remote(false);
    app.set_resize_remote(true);
    settle(&ctx, &mut app, Vec2::new(1000.0, 600.0), &mut t, 1.0).await;
    assert_eq!(wait_request(&mut server).await.0, 1000);
}

#[tokio::test]
async fn a_server_that_picks_another_size_does_not_cause_a_loop() {
    let mut server = serve(true).await;
    *server.mode.lock().unwrap() = Mode::GrantOther(800, 600);
    let (ctx, mut app) = connected(&server, None).await;
    let mut t = 100.0;
    settle(&ctx, &mut app, WINDOW, &mut t, 0.5).await;
    app.set_resize_remote(true);
    settle(&ctx, &mut app, WINDOW, &mut t, 1.0).await;
    wait_request(&mut server).await;
    settle(&ctx, &mut app, WINDOW, &mut t, 3.0).await;
    assert_eq!(
        app.remote_size(),
        (800, 600),
        "the picture follows what the server did"
    );
    assert!(
        server.requests.try_recv().is_err(),
        "the window size was asked for once, not chased"
    );
}

#[tokio::test]
async fn a_server_that_never_announced_a_layout_is_not_asked() {
    let mut server = serve(false).await;
    let (ctx, mut app) = connected(&server, None).await;
    let mut t = 100.0;
    app.set_resize_remote(true);
    settle(&ctx, &mut app, WINDOW, &mut t, 2.0).await;
    assert!(!app.server_can_resize());
    assert!(server.requests.try_recv().is_err());
}

#[tokio::test]
async fn the_choice_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("viewer.toml");
    let server = serve(true).await;
    let (ctx, mut app) = connected(&server, Some(&path)).await;
    app.set_resize_remote(true);
    frame(&ctx, &mut app, WINDOW, 200.0);
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("resize_remote = true")
    );
    drop(app);
    let again = ViewerApp::with_context(
        &ctx,
        Startup {
            settings_path: Some(path),
            ..Startup::default()
        },
    );
    assert!(again.resize_remote());
}
