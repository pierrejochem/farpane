//! A server that wants a password the user has not typed: the window asks,
//! and connects again with the answer.

use std::time::Duration;

use egui::{Context, Pos2, RawInput, Rect as UiRect, Vec2};
use farpane_viewer::app::{Startup, ViewerApp};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::net::{TcpListener, TcpStream};

fn frame(ctx: &Context, app: &mut ViewerApp) {
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 720.0))),
        focused: true,
        ..RawInput::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

async fn until(ctx: &Context, app: &mut ViewerApp, what: &str, cond: impl Fn(&ViewerApp) -> bool) {
    for _ in 0..600 {
        frame(ctx, app);
        if cond(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A VNC Auth server; returns its address.
async fn serve(password: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let opts = ServerOptions::new(
                    ServerAuth::VncPassword(password.to_vec()),
                    64,
                    48,
                    b"Locked PC",
                );
                if let Ok(mut session) = ServerSession::<TcpStream>::accept(sock, opts).await {
                    while session.next_message().await.is_ok() {}
                }
            });
        }
    });
    addr
}

fn startup(addr: String) -> Startup {
    Startup {
        addr: Some(addr),
        // Never touch the real settings.
        settings_path: None,
        ..Startup::default()
    }
}

#[tokio::test]
async fn a_missing_password_is_asked_for_and_the_answer_connects() {
    let addr = serve(b"hunter22").await;
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(addr));
    until(&ctx, &mut app, "the password question", |a| {
        a.pending_credentials().is_some()
    })
    .await;
    assert!(!app.is_connected());
    assert_eq!(
        app.pending_credentials(),
        Some(false),
        "VNC Auth asks for a password only"
    );

    app.submit_credentials("", "hunter22");
    until(&ctx, &mut app, "the connection", ViewerApp::is_connected).await;
    assert!(app.pending_credentials().is_none());
}

#[tokio::test]
async fn cancelling_the_question_stays_on_the_home_screen() {
    let addr = serve(b"hunter22").await;
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(addr));
    until(&ctx, &mut app, "the password question", |a| {
        a.pending_credentials().is_some()
    })
    .await;
    app.cancel_credentials();
    for _ in 0..30 {
        frame(&ctx, &mut app);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(app.pending_credentials().is_none());
    assert!(!app.is_connected());
}

#[tokio::test]
async fn a_wrong_password_is_an_error_not_another_question() {
    let addr = serve(b"hunter22").await;
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(addr));
    until(&ctx, &mut app, "the password question", |a| {
        a.pending_credentials().is_some()
    })
    .await;
    app.submit_credentials("", "nope");
    for _ in 0..100 {
        frame(&ctx, &mut app);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!app.is_connected());
    assert!(app.pending_credentials().is_none());
}
