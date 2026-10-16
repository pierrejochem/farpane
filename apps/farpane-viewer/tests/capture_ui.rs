//! Keyboard capture policy through the window, with a recording fake grabber.

use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use egui::{Context, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect as UiRect, Vec2};
use farpane_grab::GrabState;
use farpane_viewer::{
    app::{Startup, ViewerApp},
    capture::{CapturePref, Grabber},
};
use rfb_tokio::{ServerAuth, ServerOptions, ServerSession};
use tokio::net::{TcpListener, TcpStream};

const SCREEN: Vec2 = Vec2::new(1100.0, 720.0);

/// A grabber that records what it is asked.
#[derive(Clone, Default)]
struct Fake {
    wanted_log: Arc<Mutex<Vec<bool>>>,
    state: Arc<Mutex<Option<GrabState>>>,
}

impl Fake {
    fn last_wanted(&self) -> Option<bool> {
        self.wanted_log.lock().unwrap().last().copied()
    }

    fn report(&self, state: GrabState) {
        *self.state.lock().unwrap() = Some(state);
    }
}

struct Handle(Fake, bool);

impl Grabber for Handle {
    fn set_wanted(&mut self, wanted: bool) {
        self.0.wanted_log.lock().unwrap().push(wanted);
        self.1 = wanted;
    }

    fn poll(&mut self) -> GrabState {
        if !self.1 {
            return GrabState::Released;
        }
        self.0
            .state
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(GrabState::Active)
    }

    fn backend(&self) -> &'static str {
        "fake"
    }
}

fn frame(
    ctx: &Context,
    app: &mut ViewerApp,
    events: Vec<Event>,
    modifiers: Modifiers,
    focused: bool,
) {
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, SCREEN)),
        events,
        modifiers,
        focused,
        ..RawInput::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

fn plain(ctx: &Context, app: &mut ViewerApp) {
    frame(ctx, app, vec![], Modifiers::NONE, true);
}

async fn until(ctx: &Context, app: &mut ViewerApp, what: &str, cond: impl Fn(&ViewerApp) -> bool) {
    for _ in 0..600 {
        plain(ctx, app);
        if cond(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

struct Setup {
    ctx: Context,
    app: ViewerApp,
    fake: Fake,
    server: ServerSession<TcpStream>,
}

async fn connected(settings: Option<&std::path::Path>) -> Setup {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        ServerSession::accept(sock, ServerOptions::new(ServerAuth::None, 64, 48, b"cap"))
            .await
            .unwrap()
    });
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(
        &ctx,
        Startup {
            addr: Some(addr),
            settings_path: settings.map(std::path::Path::to_path_buf),
            ..Startup::default()
        },
    );
    let fake = Fake::default();
    app.set_grabber(Box::new(Handle(fake.clone(), false)));
    let server = server.await.unwrap();
    until(&ctx, &mut app, "connection", ViewerApp::is_connected).await;
    for _ in 0..3 {
        plain(&ctx, &mut app);
    }
    Setup {
        ctx,
        app,
        fake,
        server,
    }
}

fn key(key: Key, pressed: bool) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

fn toggle_fullscreen(s: &mut Setup) {
    frame(
        &s.ctx,
        &mut s.app,
        vec![key(Key::F11, true), key(Key::F11, false)],
        Modifiers::NONE,
        true,
    );
    plain(&s.ctx, &mut s.app);
}

const ALL: Modifiers = Modifiers {
    ctrl: true,
    alt: true,
    shift: true,
    ..Modifiers::NONE
};

#[tokio::test]
async fn by_default_the_keyboard_is_captured_in_full_screen_only() {
    let mut s = connected(None).await;
    assert_eq!(s.fake.last_wanted(), Some(false), "windowed: not captured");
    toggle_fullscreen(&mut s);
    assert!(s.app.is_fullscreen());
    assert_eq!(s.fake.last_wanted(), Some(true), "full screen: captured");
    assert_eq!(s.app.grab_state(), &GrabState::Active);
    toggle_fullscreen(&mut s);
    assert_eq!(
        s.fake.last_wanted(),
        Some(false),
        "back to a window: released"
    );
}

#[tokio::test]
async fn losing_window_focus_releases_and_regaining_it_captures_again() {
    let mut s = connected(None).await;
    toggle_fullscreen(&mut s);
    assert_eq!(s.fake.last_wanted(), Some(true));
    frame(
        &s.ctx,
        &mut s.app,
        vec![Event::WindowFocused(false)],
        Modifiers::NONE,
        false,
    );
    assert_eq!(
        s.fake.last_wanted(),
        Some(false),
        "an unfocused window must not hold the keyboard"
    );
    frame(
        &s.ctx,
        &mut s.app,
        vec![Event::WindowFocused(true)],
        Modifiers::NONE,
        true,
    );
    assert_eq!(s.fake.last_wanted(), Some(true));
}

#[tokio::test]
async fn ending_the_connection_gives_the_keyboard_back() {
    let mut s = connected(None).await;
    toggle_fullscreen(&mut s);
    assert_eq!(s.fake.last_wanted(), Some(true));
    drop(s.server);
    until(&s.ctx, &mut s.app, "disconnect", |a| !a.is_connected()).await;
    assert_eq!(s.fake.last_wanted(), Some(false));
}

#[tokio::test]
async fn the_release_chord_gives_it_back_and_a_click_takes_it_again() {
    let mut s = connected(None).await;
    toggle_fullscreen(&mut s);
    // Ctrl+Alt+Shift down, then up, with no other key.
    frame(&s.ctx, &mut s.app, vec![], ALL, true);
    assert_eq!(
        s.fake.last_wanted(),
        Some(true),
        "still captured while the chord is held"
    );
    frame(&s.ctx, &mut s.app, vec![], Modifiers::NONE, true);
    assert!(s.app.released_by_user());
    assert_eq!(s.fake.last_wanted(), Some(false));
    assert!(
        s.app.notice().is_some_and(|n| n.contains("released")),
        "{:?}",
        s.app.notice()
    );

    // Clicking the picture takes the keyboard again.
    let at = Pos2::new(SCREEN.x / 2.0, SCREEN.y / 2.0);
    let click = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame(
        &s.ctx,
        &mut s.app,
        vec![Event::PointerMoved(at)],
        Modifiers::NONE,
        true,
    );
    frame(&s.ctx, &mut s.app, vec![click(true)], Modifiers::NONE, true);
    frame(
        &s.ctx,
        &mut s.app,
        vec![click(false)],
        Modifiers::NONE,
        true,
    );
    plain(&s.ctx, &mut s.app);
    assert!(!s.app.released_by_user());
    assert_eq!(s.fake.last_wanted(), Some(true));
}

#[tokio::test]
async fn the_statistics_hotkey_does_not_count_as_the_release_chord() {
    let mut s = connected(None).await;
    toggle_fullscreen(&mut s);
    frame(&s.ctx, &mut s.app, vec![], ALL, true);
    frame(
        &s.ctx,
        &mut s.app,
        vec![key(Key::S, true), key(Key::S, false)],
        ALL,
        true,
    );
    frame(&s.ctx, &mut s.app, vec![], Modifiers::NONE, true);
    assert!(!s.app.released_by_user());
    assert_eq!(s.fake.last_wanted(), Some(true));
    assert!(s.app.stats_visible(), "the hotkey did its own job");
}

#[tokio::test]
async fn always_captures_in_a_window_and_never_does_not_even_in_full_screen() {
    let mut s = connected(None).await;
    s.app.set_capture_pref(CapturePref::Always);
    plain(&s.ctx, &mut s.app);
    assert_eq!(s.fake.last_wanted(), Some(true));
    s.app.set_capture_pref(CapturePref::Never);
    toggle_fullscreen(&mut s);
    assert!(s.app.is_fullscreen());
    assert_eq!(s.fake.last_wanted(), Some(false));
}

#[tokio::test]
async fn starting_to_capture_tells_the_user_how_to_get_out() {
    let mut s = connected(None).await;
    toggle_fullscreen(&mut s);
    assert!(
        s.app.notice().is_some_and(|n| n.contains("Ctrl+Alt+Shift")),
        "{:?}",
        s.app.notice()
    );
}

#[tokio::test]
async fn a_failed_capture_is_reported_and_the_session_carries_on() {
    let mut s = connected(None).await;
    s.fake
        .report(GrabState::Failed("the compositor said no".into()));
    toggle_fullscreen(&mut s);
    assert_eq!(
        s.app.grab_state(),
        &GrabState::Failed("the compositor said no".into())
    );
    assert!(
        s.app
            .notice()
            .is_some_and(|n| n.contains("Could not capture")),
        "{:?}",
        s.app.notice()
    );
    assert!(s.app.is_connected());
}

#[tokio::test]
async fn the_preference_is_saved_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("viewer.toml");
    let mut s = connected(Some(&path)).await;
    s.app.set_capture_pref(CapturePref::Never);
    plain(&s.ctx, &mut s.app);
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("[input]") && text.contains("capture = \"never\""),
        "{text}"
    );
    drop(s);

    let ctx = Context::default();
    let again = ViewerApp::with_context(
        &ctx,
        Startup {
            settings_path: Some(path),
            ..Startup::default()
        },
    );
    assert_eq!(again.capture_pref(), CapturePref::Never);
}

#[tokio::test]
async fn a_damaged_or_unknown_value_means_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("viewer.toml");
    fs::write(&path, "[input]\ncapture = \"sometimes\"\n").unwrap();
    let ctx = Context::default();
    let app = ViewerApp::with_context(
        &ctx,
        Startup {
            settings_path: Some(path),
            ..Startup::default()
        },
    );
    assert_eq!(app.capture_pref(), CapturePref::Auto);
}
