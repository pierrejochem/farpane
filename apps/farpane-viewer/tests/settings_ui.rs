//! Saved settings and the address book, through the window.

use std::{fs, path::PathBuf, time::Duration};

use egui::{Context, Pos2, RawInput, Rect as UiRect, Vec2};
use farpane_ui::{Accent, Theme, ThemePreference, Tokens};
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

/// A server that accepts every connection it gets, one session at a time.
async fn serve(name: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let session: ServerSession<TcpStream> = ServerSession::accept(
                sock,
                ServerOptions::new(ServerAuth::None, 64, 48, name.as_bytes()),
            )
            .await
            .unwrap();
            // Hold the session until the client goes away.
            let mut session = session;
            while session.next_message().await.is_ok() {}
        }
    });
    (addr, task)
}

fn startup(path: &std::path::Path) -> Startup {
    Startup {
        settings_path: Some(path.to_path_buf()),
        ..Startup::default()
    }
}

fn settings_file(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("farpane").join("viewer.toml")
}

#[tokio::test]
async fn saved_appearance_is_applied_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_file(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "version = 1\n[appearance]\ntheme = \"slate\"\naccent = \"cyan\"\ninterface_size = 115\nreduce_transparency = false\n",
    )
    .unwrap();
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(&path));
    frame(&ctx, &mut app);
    frame(&ctx, &mut app); // egui applies a new zoom factor at the start of the next pass
    assert_eq!(app.theme(), ThemePreference::Slate);
    let want = Tokens::new(Theme::Slate, Accent::Cyan);
    assert_eq!(ctx.style().visuals.panel_fill, egui::Color32::from(want.bg));
    assert!(
        (ctx.zoom_factor() - 1.15).abs() < 1e-6,
        "{}",
        ctx.zoom_factor()
    );
}

#[tokio::test]
async fn unknown_values_fall_back_to_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_file(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "[appearance]\ntheme = \"neon\"\ninterface_size = 250\n",
    )
    .unwrap();
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(&path));
    frame(&ctx, &mut app);
    frame(&ctx, &mut app);
    assert_eq!(app.theme(), ThemePreference::Glass);
    assert!((ctx.zoom_factor() - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn a_successful_connection_is_remembered_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_file(&dir);
    let (addr, _server) = serve("Office PC").await;
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(
        &ctx,
        Startup {
            addr: Some(addr.clone()),
            ..startup(&path)
        },
    );
    until(&ctx, &mut app, "connection", ViewerApp::is_connected).await;
    assert_eq!(
        app.saved_connections(),
        [("Office PC".to_owned(), addr.clone())]
    );
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("Office PC") && text.contains(&addr), "{text}");

    // Connecting again through the saved card updates it instead of adding a second one.
    drop(app);
    let mut again = ViewerApp::with_context(&ctx, startup(&path));
    assert_eq!(again.saved_connections().len(), 1, "loaded from disk");
    again.connect_saved(0);
    until(&ctx, &mut again, "reconnect", ViewerApp::is_connected).await;
    assert_eq!(again.saved_connections().len(), 1);
}

#[tokio::test]
async fn failed_connections_are_not_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_file(&dir);
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().to_string()
    };
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(
        &ctx,
        Startup {
            addr: Some(closed),
            ..startup(&path)
        },
    );
    for _ in 0..100 {
        frame(&ctx, &mut app);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!app.is_connected());
    assert_eq!(app.saved_connections(), []);
    assert!(!path.exists(), "nothing to save");
}

#[tokio::test]
async fn changing_the_theme_is_saved_with_the_next_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_file(&dir);
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(&path));
    frame(&ctx, &mut app);
    assert!(!path.exists(), "unchanged defaults are not written");
    app.set_theme(ThemePreference::Light);
    frame(&ctx, &mut app);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("theme = \"light\""), "{text}");
    assert_eq!(
        ctx.style().visuals.panel_fill,
        egui::Color32::from(Tokens::new(Theme::Light, Accent::ThemeDefault).bg)
    );
}

#[tokio::test]
async fn forgetting_a_connection_removes_it_from_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_file(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "[[connection]]\nname = \"A\"\naddr = \"a:5900\"\nlast_connected = 10\n[[connection]]\nname = \"B\"\naddr = \"b:5900\"\nlast_connected = 20\n",
    )
    .unwrap();
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(&path));
    assert_eq!(
        app.saved_connections(),
        [("B".into(), "b:5900".into()), ("A".into(), "a:5900".into())],
        "most recent first"
    );
    app.forget_saved("B:5900");
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("a:5900") && !text.contains("b:5900"),
        "{text}"
    );
}

#[tokio::test]
async fn a_damaged_settings_file_does_not_stop_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_file(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "not = [toml").unwrap();
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(&path));
    frame(&ctx, &mut app);
    assert!(
        app.notice().is_some_and(|n| n.contains("damaged")),
        "{:?}",
        app.notice()
    );
    assert!(path.with_extension("toml.broken").exists());
    assert_eq!(app.theme(), ThemePreference::Glass);
}

#[tokio::test]
async fn without_a_settings_path_nothing_touches_the_disk() {
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, Startup::default());
    app.set_theme(ThemePreference::Slate);
    frame(&ctx, &mut app);
    assert!(app.notice().is_none(), "no 'could not save' noise either");
}
