//! The top bar folds into a menu in a narrow window, and the footer can be
//! switched off.

use egui::{Context, Pos2, RawInput, Rect as UiRect, Vec2};
use farpane_viewer::app::{Startup, ViewerApp};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::net::TcpListener;

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

fn frame(ctx: &Context, app: &mut ViewerApp, width: f32) -> egui::FullOutput {
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, Vec2::new(width, 720.0))),
        focused: true,
        ..RawInput::default()
    };
    ctx.run(input, |ctx| app.ui(ctx))
}

async fn connected() -> (Context, ViewerApp, ServerSession<tokio::net::TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        ServerSession::accept(
            sock,
            ServerOptions::new(ServerAuth::None, 800, 400, b"header test"),
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
    for _ in 0..600 {
        frame(&ctx, &mut app, 1100.0);
        if app.is_connected() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(app.is_connected());
    for _ in 0..3 {
        frame(&ctx, &mut app, 1100.0);
    }
    (ctx, app, server)
}

#[tokio::test]
async fn a_narrow_window_folds_the_session_controls_into_a_menu() {
    let (ctx, mut app, _server) = connected().await;
    let wide = drawn_text(&frame(&ctx, &mut app, 1100.0));
    assert!(
        wide.contains("Ctrl+Alt+Del") && wide.contains("Capture keys"),
        "{wide}"
    );
    assert!(!wide.contains("More"), "{wide}");
    let narrow = drawn_text(&frame(&ctx, &mut app, 800.0));
    assert!(
        narrow.contains("More") && narrow.contains("Disconnect"),
        "{narrow}"
    );
    assert!(
        !narrow.contains("Ctrl+Alt+Del") && !narrow.contains("Capture keys"),
        "{narrow}"
    );
    assert!(narrow.contains("Settings"), "{narrow}");
}

#[tokio::test]
async fn the_footer_can_be_switched_off() {
    let (ctx, mut app, _server) = connected().await;
    assert!(app.show_footer(), "on by default");
    let on = drawn_text(&frame(&ctx, &mut app, 1100.0));
    assert!(on.contains("RFB 3.8"), "{on}");
    app.set_show_footer(false);
    let off = drawn_text(&frame(&ctx, &mut app, 1100.0));
    assert!(!off.contains("RFB 3.8"), "{off}");
}
