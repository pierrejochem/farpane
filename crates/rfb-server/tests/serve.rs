//! The server against a client built from `rfb-tokio` and `rfb-codec`.

use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use rfb_codec::{
    Damage, Decoder, Framebuffer, PixelConverter, PixelPacker,
    clipboard::{ExtClipboard, Role},
};
use rfb_proto::{
    PixelFormat,
    conn::ClientConnEvent,
    consts::{enc, fence, pseudo},
    msg::client::ClientMessage,
    types::{Rect, Screen},
};
use rfb_server::{Capture, Input, NoInput, Server, Settings, Source, TestPattern};
use rfb_tokio::{ClientOptions, ClientSession, Error, ServerAuth, ServerInfo, ServerOptions};
use tokio::{
    io::{DuplexStream, duplex},
    time::timeout,
};

fn options() -> ServerOptions {
    ServerOptions::new(ServerAuth::None, 1, 1, b"test server")
}

/// A capture source the test steers.
#[derive(Debug, Clone, Default)]
struct Script(Arc<Mutex<Plan>>);

#[derive(Debug, Default)]
struct Plan {
    damage: Vec<Rect>,
    resize: Option<(u16, u16)>,
    fail: bool,
    shade: u8,
    cursor: Option<rfb_codec::Cursor>,
    clipboard: Option<String>,
}

struct Scripted {
    plan: Script,
    size: (u16, u16),
}

impl Source for Scripted {
    fn size(&self) -> (u16, u16) {
        self.size
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let mut plan = self.plan.0.lock().unwrap();
        if plan.fail {
            return Err(io::Error::other("the screen went away"));
        }
        let whole = |fb: &Framebuffer| Rect {
            x: 0,
            y: 0,
            width: fb.width(),
            height: fb.height(),
        };
        let shade = plan.shade;
        let paint = |fb: &mut Framebuffer, rect: Rect| {
            let data =
                vec![[shade, shade, shade, 0]; usize::from(rect.width) * usize::from(rect.height)];
            fb.write_bgrx(rect, data.as_flattened()).unwrap();
        };
        if let Some((w, h)) = plan.resize.take() {
            fb.resize(w, h).unwrap();
            let r = whole(fb);
            paint(fb, r);
            return Ok(Capture {
                damage: vec![r],
                cursor: None,
                clipboard: None,
            });
        }
        let cursor = plan.cursor.take();
        let clipboard = plan.clipboard.take();
        let damage = std::mem::take(&mut plan.damage);
        for r in &damage {
            paint(fb, *r);
        }
        Ok(Capture {
            damage,
            cursor,
            clipboard,
        })
    }
}

fn scripted(size: (u16, u16)) -> (Scripted, Script) {
    let plan = Script::default();
    (
        Scripted {
            plan: plan.clone(),
            size,
        },
        plan,
    )
}

#[derive(Debug, Default)]
struct Recorder(Mutex<Vec<String>>);

#[derive(Debug, Clone, Default)]
struct Recording(Arc<Recorder>);

impl Input for Recording {
    fn key(&self, keysym: u32, down: bool) {
        self.0
            .0
            .lock()
            .unwrap()
            .push(format!("key {keysym:x} {down}"));
    }
    fn pointer(&self, x: u16, y: u16, buttons: u8) {
        self.0
            .0
            .lock()
            .unwrap()
            .push(format!("pointer {x},{y} {buttons}"));
    }
    fn clipboard(&self, text: &str) {
        self.0.0.lock().unwrap().push(format!("clipboard {text:?}"));
    }
}

impl Recording {
    fn seen(&self) -> Vec<String> {
        self.0.0.lock().unwrap().clone()
    }
}

/// A viewer: a session, a decoder and a picture.
struct Viewer {
    session: ClientSession<DuplexStream>,
    decoder: Decoder,
    fb: Framebuffer,
    pf: PixelFormat,
    info: ServerInfo,
    /// What the server sent outside updates.
    other: Vec<ClientConnEvent>,
    layouts: Vec<rfb_codec::DesktopLayout>,
    cursors: Vec<rfb_codec::Cursor>,
}

async fn connect(
    server: &Arc<Server>,
    pf: PixelFormat,
    encodings: &[i32],
) -> (Viewer, tokio::task::JoinHandle<Result<(), Error>>) {
    connect_with(server, pf, encodings, ClientOptions::default()).await
}

async fn connect_with(
    server: &Arc<Server>,
    pf: PixelFormat,
    encodings: &[i32],
    options: ClientOptions,
) -> (Viewer, tokio::task::JoinHandle<Result<(), Error>>) {
    let (near, far) = duplex(1 << 20);
    let s = Arc::clone(server);
    let task = tokio::spawn(async move { s.serve_connection(far).await });
    let (mut session, info) = ClientSession::connect(near, options).await.unwrap();
    session
        .send(&ClientMessage::SetPixelFormat(pf))
        .await
        .unwrap();
    session
        .send(&ClientMessage::SetEncodings(encodings.to_vec()))
        .await
        .unwrap();
    let viewer = Viewer {
        decoder: Decoder::new(&pf).unwrap(),
        fb: Framebuffer::new(info.width, info.height).unwrap(),
        session,
        pf,
        info,
        other: Vec::new(),
        layouts: Vec::new(),
        cursors: Vec::new(),
    };
    (viewer, task)
}

impl Viewer {
    async fn request(&mut self, incremental: bool) {
        let rect = Rect {
            x: 0,
            y: 0,
            width: self.fb.width(),
            height: self.fb.height(),
        };
        self.session
            .send(&ClientMessage::FramebufferUpdateRequest { incremental, rect })
            .await
            .unwrap();
    }

    /// Read one whole update, applying it; returns the rectangles' headers.
    async fn update(&mut self) -> Vec<(Rect, i32)> {
        let mut rects = Vec::new();
        let mut inside = false;
        loop {
            match timeout(Duration::from_secs(5), self.session.next_event())
                .await
                .expect("the server answered")
                .unwrap()
            {
                ClientConnEvent::UpdateBegin { .. } => inside = true,
                ClientConnEvent::Rect { header, payload } => {
                    match self
                        .decoder
                        .decode(&mut self.fb, &header, &payload)
                        .unwrap()
                    {
                        Damage::Layout(l) => self.layouts.push(l),
                        Damage::Cursor(c) => self.cursors.push(c),
                        _ => rects.push((header.rect, header.encoding)),
                    }
                }
                ClientConnEvent::UpdateEnd => return rects,
                other => {
                    assert!(!inside, "{other:?} inside an update");
                    self.other.push(other);
                }
            }
        }
    }

    /// Wait up to `span` for any event outside updates.
    async fn quiet(&mut self, span: Duration) {
        while let Ok(event) = timeout(span, self.session.next_event()).await {
            self.other.push(event.unwrap());
        }
    }

    fn matches(&self, rect: Rect, f: impl Fn(u16, u16) -> [u8; 4]) {
        let conv = PixelConverter::new(&self.pf).unwrap();
        let packer = PixelPacker::new(&self.pf).unwrap();
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                let mut wire = Vec::new();
                let [b, g, r, _] = f(x, y);
                packer.pack([b, g, r, 0xFF], &mut wire);
                assert_eq!(
                    self.fb.pixel(x, y),
                    Some(conv.pixel(&wire)),
                    "pixel {x},{y}"
                );
            }
        }
    }
}

const ZRLE: &[i32] = &[enc::ZRLE, enc::RAW, pseudo::DESKTOP_SIZE];
const RAW: &[i32] = &[enc::RAW];
const TRLE: &[i32] = &[enc::TRLE, enc::RAW];

fn rgb565() -> PixelFormat {
    PixelFormat {
        bits_per_pixel: 16,
        depth: 16,
        red_max: 31,
        green_max: 63,
        blue_max: 31,
        red_shift: 11,
        green_shift: 5,
        blue_shift: 0,
        ..PixelFormat::RGB888
    }
}

fn whole(w: u16, h: u16) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    }
}

#[tokio::test]
async fn the_first_request_gets_the_whole_picture_in_the_clients_format_and_encoding() {
    let server = Arc::new(
        Server::new(
            TestPattern::new(150, 90),
            NoInput,
            options(),
            Settings::default(),
        )
        .unwrap(),
    );
    for (pf, encodings) in [
        (PixelFormat::RGB888, ZRLE),
        (PixelFormat::RGB888, RAW),
        (rgb565(), ZRLE),
        (rgb565(), RAW),
        (PixelFormat::RGB888, TRLE),
        (rgb565(), TRLE),
    ] {
        let (mut v, _task) = connect(&server, pf, encodings).await;
        assert_eq!((v.info.width, v.info.height), (150, 90));
        assert_eq!(v.info.name, b"test server");
        v.request(false).await;
        let rects = v.update().await;
        assert_ne!(rects.len(), 0);
        assert!(rects.iter().all(|&(_, e)| e == encodings[0]), "{rects:?}");
        // Every pixel is the background or the white square, and the square
        // is whole (the capture thread keeps moving it).
        let mut white = 0;
        for y in 0..90 {
            for x in 0..150 {
                let px = v.fb.pixel(x, y).unwrap();
                if px == [255, 255, 255, 255] && v.pf == PixelFormat::RGB888 {
                    white += 1;
                } else if px != [255, 255, 255, 255] {
                    let one = Rect {
                        x,
                        y,
                        width: 1,
                        height: 1,
                    };
                    v.matches(one, TestPattern::background);
                } else {
                    white += 1; // 16 bit white is also all ones
                }
            }
        }
        // The square appears with the second frame, which may not have been taken yet.
        assert!(
            white == 0 || white == 22 * 22,
            "a whole square or none: {white}"
        );
    }
}

#[tokio::test]
async fn an_incremental_request_waits_for_a_change_and_gets_only_that() {
    let (source, plan) = scripted((200, 100));
    let server = Arc::new(
        Server::new(
            source,
            NoInput,
            options(),
            Settings {
                fps: 100,
                view_only: false,
            },
        )
        .unwrap(),
    );
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    v.update().await;

    v.request(true).await;
    assert!(
        timeout(Duration::from_millis(300), v.update())
            .await
            .is_err(),
        "nothing changed, so nothing is sent"
    );
    // The request is still waiting: a change answers it.
    let changed = Rect {
        x: 40,
        y: 30,
        width: 20,
        height: 10,
    };
    {
        let mut p = plan.0.lock().unwrap();
        p.shade = 200;
        p.damage = vec![changed];
    }
    let rects = v.update().await;
    assert_eq!(rects.iter().map(|r| r.0).collect::<Vec<_>>(), [changed]);
    v.matches(changed, |_, _| [200, 200, 200, 0]);
    v.matches(
        Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        },
        |_, _| [0, 0, 0, 0],
    );
}

#[tokio::test]
async fn changes_outside_the_requested_area_are_kept_for_later() {
    let (source, plan) = scripted((200, 100));
    let server = Arc::new(
        Server::new(
            source,
            NoInput,
            options(),
            Settings {
                fps: 100,
                view_only: false,
            },
        )
        .unwrap(),
    );
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    v.update().await;
    {
        let mut p = plan.0.lock().unwrap();
        p.shade = 90;
        p.damage = vec![Rect {
            x: 150,
            y: 50,
            width: 10,
            height: 10,
        }];
    }
    // Ask only about the left half: the change is on the right.
    v.session
        .send(&ClientMessage::FramebufferUpdateRequest {
            incremental: true,
            rect: Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
        })
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(300), v.update())
            .await
            .is_err()
    );
    // Now ask about everything: the change arrives.
    v.request(true).await;
    let rects = v.update().await;
    assert_eq!(rects.len(), 1);
    v.matches(rects[0].0, |_, _| [90, 90, 90, 0]);
}

#[tokio::test]
async fn input_reaches_the_backend_and_view_only_stops_it() {
    for view_only in [false, true] {
        let input = Recording::default();
        let server = Arc::new(
            Server::new(
                TestPattern::new(64, 64),
                input.clone(),
                options(),
                Settings { fps: 30, view_only },
            )
            .unwrap(),
        );
        let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
        v.session
            .send(&ClientMessage::KeyEvent {
                down: true,
                keysym: 0xFF0D,
            })
            .await
            .unwrap();
        v.session
            .send(&ClientMessage::PointerEvent {
                buttons: 1,
                x: 5,
                y: 6,
            })
            .await
            .unwrap();
        v.session
            .send(&ClientMessage::ClientCutText(b"caf\xe9".to_vec()))
            .await
            .unwrap();
        // Give the server time to handle them.
        v.quiet(Duration::from_millis(200)).await;
        let seen = input.seen();
        if view_only {
            assert!(seen.is_empty(), "{seen:?}");
        } else {
            assert_eq!(
                seen,
                ["key ff0d true", "pointer 5,6 1", "clipboard \"caf\u{e9}\""]
            );
        }
    }
}

#[tokio::test]
async fn utf8_clipboard_works_through_extended_clipboard() {
    let input = Recording::default();
    let server = Arc::new(
        Server::new(
            TestPattern::new(64, 64),
            input.clone(),
            options(),
            Settings::default(),
        )
        .unwrap(),
    );
    let (mut v, _task) = connect(
        &server,
        PixelFormat::RGB888,
        &[enc::ZRLE, pseudo::EXT_CLIPBOARD],
    )
    .await;
    let mut clip = ExtClipboard::new(Role::Client);
    // The server announces itself right after SetEncodings; answer it.
    let mut sent = false;
    for _ in 0..50 {
        let Ok(event) = timeout(Duration::from_millis(200), v.session.next_event()).await else {
            break;
        };
        if let ClientConnEvent::ExtendedClipboard { flags, payload } = event.unwrap() {
            for (flags, payload) in clip.on_message(flags, &payload).unwrap().send {
                v.session
                    .send(&ClientMessage::ExtendedClipboard { flags, payload })
                    .await
                    .unwrap();
            }
            sent = true;
            break;
        }
    }
    assert!(sent, "the server did not announce Extended Clipboard");
    assert!(clip.is_active());
    // Notify, then answer the server's request with the text.
    for (flags, payload) in clip.local_changed("5\u{20ac} \u{4f60}\u{597d}\nline") {
        v.session
            .send(&ClientMessage::ExtendedClipboard { flags, payload })
            .await
            .unwrap();
    }
    for _ in 0..50 {
        let Ok(event) = timeout(Duration::from_millis(200), v.session.next_event()).await else {
            break;
        };
        if let ClientConnEvent::ExtendedClipboard { flags, payload } = event.unwrap() {
            for (flags, payload) in clip.on_message(flags, &payload).unwrap().send {
                v.session
                    .send(&ClientMessage::ExtendedClipboard { flags, payload })
                    .await
                    .unwrap();
            }
        }
        if !input.seen().is_empty() {
            break;
        }
    }
    v.quiet(Duration::from_millis(200)).await;
    assert_eq!(
        input.seen(),
        ["clipboard \"5\u{20ac} \u{4f60}\u{597d}\\nline\""],
        "LF line ends"
    );
}

#[tokio::test]
async fn fence_is_announced_and_requests_are_answered_with_the_known_flags() {
    let server = Arc::new(
        Server::new(
            TestPattern::new(64, 64),
            NoInput,
            options(),
            Settings::default(),
        )
        .unwrap(),
    );
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, &[enc::RAW, pseudo::FENCE]).await;
    v.quiet(Duration::from_millis(200)).await;
    assert!(
        v.other
            .iter()
            .any(|e| matches!(e, ClientConnEvent::Fence { .. })),
        "{:?}",
        v.other
    );
    v.other.clear();
    v.session
        .send(&ClientMessage::Fence {
            flags: fence::REQUEST | fence::BLOCK_BEFORE | (1 << 9),
            payload: vec![1, 2, 3],
        })
        .await
        .unwrap();
    v.quiet(Duration::from_millis(200)).await;
    assert_eq!(
        v.other,
        [ClientConnEvent::Fence {
            flags: fence::BLOCK_BEFORE,
            payload: vec![1, 2, 3]
        }]
    );
}

#[tokio::test]
async fn a_resize_is_announced_to_clients_that_can_follow_and_ends_others() {
    let (source, plan) = scripted((100, 80));
    let server = Arc::new(
        Server::new(
            source,
            NoInput,
            options(),
            Settings {
                fps: 100,
                view_only: false,
            },
        )
        .unwrap(),
    );
    let (mut ext, _ext_task) = connect(
        &server,
        PixelFormat::RGB888,
        &[enc::ZRLE, pseudo::EXT_DESKTOP_SIZE],
    )
    .await;
    let (mut legacy, _legacy_task) = connect(
        &server,
        PixelFormat::RGB888,
        &[enc::ZRLE, pseudo::DESKTOP_SIZE],
    )
    .await;
    let (mut stuck, t3) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE]).await;
    for v in [&mut ext, &mut legacy, &mut stuck] {
        v.request(false).await;
        v.update().await;
    }
    assert_eq!(
        ext.layouts.len(),
        1,
        "the extended client is told the layout at once"
    );
    assert_eq!(legacy.layouts.len(), 0);

    {
        let mut p = plan.0.lock().unwrap();
        p.shade = 120;
        p.resize = Some((160, 90));
    }
    for v in [&mut ext, &mut legacy] {
        v.request(true).await;
        v.update().await;
        assert_eq!((v.fb.width(), v.fb.height()), (160, 90));
        v.matches(whole(160, 90), |_, _| [120, 120, 120, 0]);
    }
    assert_eq!(ext.layouts.last().unwrap().screens.len(), 1);
    assert_eq!(
        (
            ext.layouts.last().unwrap().width,
            ext.layouts.last().unwrap().height
        ),
        (160, 90)
    );
    // The client without a size encoding cannot show the new picture.
    stuck.request(true).await;
    let ended = timeout(Duration::from_secs(5), t3).await.unwrap().unwrap();
    assert!(ended.is_err());
}

#[tokio::test]
async fn set_desktop_size_is_refused_with_the_next_update() {
    let server = Arc::new(
        Server::new(
            TestPattern::new(64, 64),
            NoInput,
            options(),
            Settings::default(),
        )
        .unwrap(),
    );
    let (mut v, _task) = connect(
        &server,
        PixelFormat::RGB888,
        &[enc::ZRLE, pseudo::EXT_DESKTOP_SIZE],
    )
    .await;
    v.request(false).await;
    v.update().await;
    v.session
        .send(&ClientMessage::SetDesktopSize {
            width: 800,
            height: 600,
            screens: vec![Screen {
                id: 0,
                x: 0,
                y: 0,
                width: 800,
                height: 600,
                flags: 0,
            }],
        })
        .await
        .unwrap();
    v.request(true).await;
    v.update().await;
    assert_eq!((v.fb.width(), v.fb.height()), (64, 64));
    let reply = v.layouts.last().unwrap();
    assert_eq!(
        (reply.reason, reply.status),
        (1, 1),
        "client-initiated, prohibited"
    );
}

#[tokio::test]
async fn a_dead_screen_disconnects_everyone() {
    let (source, plan) = scripted((64, 64));
    let server = Arc::new(
        Server::new(
            source,
            NoInput,
            options(),
            Settings {
                fps: 100,
                view_only: false,
            },
        )
        .unwrap(),
    );
    let (_v, task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    plan.0.lock().unwrap().fail = true;
    let ended = timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(ended.is_err());
    assert_eq!(
        server.capture_failure().as_deref(),
        Some("the screen went away")
    );
}

#[tokio::test]
async fn vnc_password_authentication_and_the_client_count() {
    let opts = ServerOptions::new(ServerAuth::VncPassword(b"secret".to_vec()), 1, 1, b"locked");
    let server = Arc::new(
        Server::new(TestPattern::new(64, 64), NoInput, opts, Settings::default()).unwrap(),
    );
    assert_eq!(server.client_count(), 0);
    let (near, far) = duplex(1 << 16);
    let s = Arc::clone(&server);
    let bad = tokio::spawn(async move { s.serve_connection(far).await });
    let wrong = ClientOptions {
        password: Some(b"nope".to_vec()),
        ..ClientOptions::default()
    };
    assert!(ClientSession::connect(near, wrong).await.is_err());
    assert!(matches!(bad.await.unwrap(), Err(Error::AuthFailed)));
    assert_eq!(server.client_count(), 0);

    let right = ClientOptions {
        password: Some(b"secret".to_vec()),
        ..ClientOptions::default()
    };
    let (mut v, _task) = connect_with(&server, PixelFormat::RGB888, ZRLE, right).await;
    v.request(false).await;
    v.update().await;
    assert_eq!(server.client_count(), 1);
}

#[tokio::test]
async fn a_policy_can_make_a_client_view_only_and_events_report_the_session() {
    use rfb_server::{Event, Permission};
    let input = Recording::default();
    let opts = ServerOptions::new(ServerAuth::VncPassword(b"secret".to_vec()), 1, 1, b"x");
    let mut server = Server::new(
        TestPattern::new(64, 64),
        input.clone(),
        opts,
        Settings::default(),
    )
    .unwrap();
    server.set_policy(|user| {
        assert_eq!(user, None, "a shared password has no user name");
        Permission::ViewOnly
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    server.set_events(move |e| sink.lock().unwrap().push(e.clone()));
    let server = Arc::new(server);

    let right = ClientOptions {
        password: Some(b"secret".to_vec()),
        ..ClientOptions::default()
    };
    let (mut v, task) = connect_with(&server, PixelFormat::RGB888, ZRLE, right).await;
    v.session
        .send(&ClientMessage::KeyEvent {
            down: true,
            keysym: 0x61,
        })
        .await
        .unwrap();
    v.quiet(Duration::from_millis(200)).await;
    assert_eq!(input.seen().len(), 0, "view-only drops input");
    drop(v);
    let _ = task.await;

    let (near, far) = duplex(1 << 16);
    let s = Arc::clone(&server);
    let bad = tokio::spawn(async move { s.serve_connection(far).await });
    let wrong = ClientOptions {
        password: Some(b"nope".to_vec()),
        ..ClientOptions::default()
    };
    let _ = ClientSession::connect(near, wrong).await;
    let _ = bad.await;

    let seen = events.lock().unwrap().clone();
    assert!(
        matches!(
            seen.as_slice(),
            [
                Event::Connected {
                    user: None,
                    permission: Permission::ViewOnly,
                    ..
                },
                Event::Closed { error: None, .. },
                Event::AuthFailed { .. },
            ]
        ),
        "{seen:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_failed_to_sign_in_waits_before_the_next_attempt() {
    let opts = ServerOptions::new(ServerAuth::VncPassword(b"secret".to_vec()), 1, 1, b"x");
    let server = Arc::new(
        Server::new(TestPattern::new(64, 64), NoInput, opts, Settings::default()).unwrap(),
    );
    let peer: std::net::SocketAddr = "192.0.2.7:4000".parse().unwrap();
    let attempt = |password: &'static [u8]| {
        let server = Arc::clone(&server);
        async move {
            let (near, far) = duplex(1 << 16);
            let task = tokio::spawn(async move { server.serve(far, Some(peer)).await });
            let options = ClientOptions {
                password: Some(password.to_vec()),
                ..ClientOptions::default()
            };
            let started = tokio::time::Instant::now();
            let result = ClientSession::connect(near, options).await;
            (started.elapsed(), result.is_ok(), task)
        }
    };
    let (first, ok, _t) = attempt(b"nope").await;
    assert!(
        !ok && first < Duration::from_millis(500),
        "the first failure is not delayed: {first:?}"
    );
    let (second, ok, _t) = attempt(b"nope").await;
    assert!(!ok && second >= Duration::from_secs(1), "{second:?}");
    let (third, ok, _t) = attempt(b"nope").await;
    assert!(!ok && third >= Duration::from_secs(2), "{third:?}");
    // A correct password still waits for the delay it has earned, then clears it.
    let (fourth, ok, _t4) = attempt(b"secret").await;
    assert!(ok && fourth >= Duration::from_secs(4), "{fourth:?}");
    let (fifth, ok, _t5) = attempt(b"secret").await;
    assert!(ok && fifth < Duration::from_millis(500), "{fifth:?}");
}

#[tokio::test]
async fn the_cursor_shape_is_sent_to_clients_that_ask_and_updated_when_it_changes() {
    let (source, plan) = scripted((64, 64));
    let server = Arc::new(
        Server::new(
            source,
            NoInput,
            options(),
            Settings {
                fps: 100,
                view_only: false,
            },
        )
        .unwrap(),
    );
    // Nothing known yet: the first update has no cursor rectangle.
    let (mut v, _t1) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, pseudo::CURSOR]).await;
    v.request(false).await;
    let rects = v.update().await;
    assert!(rects.iter().all(|&(_, e)| e != pseudo::CURSOR));
    assert_eq!(v.cursors.len(), 0);

    let shape = rfb_codec::Cursor {
        hotspot: (2, 3),
        width: 4,
        height: 2,
        rgba: [[255, 0, 0, 255], [0, 255, 0, 0]].concat().repeat(4),
    };
    plan.0.lock().unwrap().cursor = Some(shape.clone());
    v.request(true).await;
    v.update().await;
    assert_eq!(v.cursors.as_slice(), std::slice::from_ref(&shape));

    // A client that did not ask never gets one; one that joins later gets the current shape at once.
    let (mut other, _other_task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    other.request(false).await;
    other.update().await;
    assert_eq!(other.cursors.len(), 0);
    let (mut late, _t3) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, pseudo::CURSOR]).await;
    late.request(false).await;
    late.update().await;
    assert_eq!(late.cursors, [shape]);

    // A hidden cursor (no pixels) arrives as an empty shape.
    plan.0.lock().unwrap().cursor = Some(rfb_codec::Cursor {
        hotspot: (0, 0),
        width: 0,
        height: 0,
        rgba: Vec::new(),
    });
    v.request(true).await;
    v.update().await;
    assert_eq!(v.cursors.last().map(|c| (c.width, c.height)), Some((0, 0)));
}

#[tokio::test]
async fn the_test_pattern_shows_an_arrow_in_every_pixel_format() {
    let server = Arc::new(
        Server::new(
            TestPattern::new(64, 64),
            NoInput,
            options(),
            Settings::default(),
        )
        .unwrap(),
    );
    for pf in [PixelFormat::RGB888, rgb565()] {
        let (mut v, _t) = connect(&server, pf, &[enc::ZRLE, pseudo::CURSOR]).await;
        v.request(false).await;
        v.update().await;
        let arrow = TestPattern::arrow();
        assert_eq!(v.cursors.len(), 1);
        let got = &v.cursors[0];
        assert_eq!((got.width, got.height, got.hotspot), (12, 12, (0, 0)));
        let alphas =
            |c: &rfb_codec::Cursor| c.rgba.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>();
        assert_eq!(alphas(got), alphas(&arrow));
    }
}

#[tokio::test]
async fn the_servers_clipboard_reaches_clients_in_latin1_or_as_utf8() {
    let (source, plan) = scripted((64, 64));
    let server = Arc::new(
        Server::new(
            source,
            NoInput,
            options(),
            Settings {
                fps: 100,
                view_only: false,
            },
        )
        .unwrap(),
    );

    // A client without Extended Clipboard gets Latin-1, with `?` for the rest.
    let (mut old, _old_task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    // One with it gets UTF-8 through notify, request, provide.
    let (mut new, _new_task) = connect(
        &server,
        PixelFormat::RGB888,
        &[enc::ZRLE, pseudo::EXT_CLIPBOARD],
    )
    .await;
    let mut clip = ExtClipboard::new(Role::Client);
    for _ in 0..50 {
        let Ok(event) = timeout(Duration::from_millis(200), new.session.next_event()).await else {
            break;
        };
        if let ClientConnEvent::ExtendedClipboard { flags, payload } = event.unwrap() {
            for (flags, payload) in clip.on_message(flags, &payload).unwrap().send {
                new.session
                    .send(&ClientMessage::ExtendedClipboard { flags, payload })
                    .await
                    .unwrap();
            }
            break;
        }
    }
    assert!(clip.is_active());

    plan.0.lock().unwrap().clipboard = Some("caf\u{e9} 5\u{20ac}\n\u{4f60}".to_owned());

    old.quiet(Duration::from_millis(400)).await;
    assert!(
        old.other
            .iter()
            .any(|e| matches!(e, ClientConnEvent::CutText(t) if t == b"caf\xe9 5?\n?")),
        "{:?}",
        old.other
    );

    let mut text = None;
    for _ in 0..50 {
        let Ok(event) = timeout(Duration::from_millis(200), new.session.next_event()).await else {
            break;
        };
        if let ClientConnEvent::ExtendedClipboard { flags, payload } = event.unwrap() {
            let reaction = clip.on_message(flags, &payload).unwrap();
            for (flags, payload) in reaction.send {
                new.session
                    .send(&ClientMessage::ExtendedClipboard { flags, payload })
                    .await
                    .unwrap();
            }
            if reaction.received.is_some() {
                text = reaction.received;
                break;
            }
        }
    }
    assert_eq!(text.as_deref(), Some("caf\u{e9} 5\u{20ac}\r\n\u{4f60}"));
}
