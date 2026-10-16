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
            match timeout(Duration::from_secs(10), self.session.next_event())
                .await
                .expect("the server answered")
                .unwrap()
            {
                ClientConnEvent::UpdateBegin { .. } => inside = true,
                ClientConnEvent::Rect { header, payload } => {
                    match self
                        .decoder
                        .decode(&mut self.fb, &header, &payload)
                        .unwrap_or_else(|e| panic!("{e:?} for {header:?}"))
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

    async fn key(&mut self, keysym: u32) {
        self.session
            .send(&ClientMessage::KeyEvent { down: true, keysym })
            .await
            .unwrap();
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
const HEXTILE: &[i32] = &[enc::HEXTILE, enc::RAW];
const TIGHT: &[i32] = &[enc::TIGHT, enc::ZRLE, enc::RAW];

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
        (PixelFormat::RGB888, HEXTILE),
        (rgb565(), HEXTILE),
        (PixelFormat::RGB888, TIGHT),
        (rgb565(), TIGHT),
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
async fn a_paused_server_sends_nothing_and_ignores_input_until_resumed() {
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
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    v.update().await;
    assert!(!server.is_paused());
    server.set_paused(true);
    assert!(server.is_paused());
    v.request(true).await;
    v.key(0x61).await;
    v.quiet(Duration::from_millis(1500)).await;
    assert!(
        !v.other
            .iter()
            .any(|e| matches!(e, ClientConnEvent::UpdateBegin { .. })),
        "no picture while paused: {:?}",
        v.other
    );
    assert!(input.seen().is_empty(), "{:?}", input.seen());
    server.set_paused(false);
    assert!(
        !v.update().await.is_empty(),
        "the picture follows on resume"
    );
}

#[tokio::test]
async fn keys_and_buttons_are_let_go_when_the_client_leaves_or_becomes_view_only() {
    for leave in [true, false] {
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
        let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
        v.session
            .send(&ClientMessage::KeyEvent {
                down: true,
                keysym: 0xFFE3,
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
        v.quiet(Duration::from_millis(200)).await;
        assert_eq!(input.seen(), ["key ffe3 true", "pointer 5,6 1"]);
        if leave {
            drop(v);
        } else {
            let id = server.clients()[0].id;
            assert!(server.set_view_only(id, true));
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let seen = input.seen();
        assert!(seen.contains(&"key ffe3 false".to_owned()), "{seen:?}");
        assert!(seen.contains(&"pointer 5,6 0".to_owned()), "{seen:?}");
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

#[tokio::test]
async fn the_clients_order_of_preference_decides_and_wide_tight_rectangles_are_split() {
    // 3000 pixels wide: Tight has to cut it in two.
    let server = Arc::new(
        Server::new(
            TestPattern::new(3000, 40),
            NoInput,
            options(),
            Settings::default(),
        )
        .unwrap(),
    );
    let (mut v, _t1) = connect(
        &server,
        PixelFormat::RGB888,
        &[enc::TIGHT, enc::ZRLE, enc::RAW],
    )
    .await;
    v.request(false).await;
    let rects = v.update().await;
    assert!(rects.iter().all(|&(_, e)| e == enc::TIGHT), "{rects:?}");
    assert!(rects.iter().all(|&(r, _)| r.width <= 2048), "{rects:?}");
    let area: u32 = rects
        .iter()
        .map(|&(r, _)| u32::from(r.width) * u32::from(r.height))
        .sum();
    assert_eq!(area, 3000 * 40, "the pieces cover the screen exactly");
    v.matches(
        Rect {
            x: 2990,
            y: 30,
            width: 10,
            height: 10,
        },
        TestPattern::background,
    );
    // The same client listing ZRLE first gets ZRLE.
    let (mut z, _t2) = connect(
        &server,
        PixelFormat::RGB888,
        &[enc::ZRLE, enc::TIGHT, enc::RAW],
    )
    .await;
    z.request(false).await;
    assert!(z.update().await.iter().all(|&(_, e)| e == enc::ZRLE));
}

#[tokio::test]
async fn the_dashboard_can_see_clients_switch_view_only_and_disconnect_them() {
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
    assert_eq!(server.clients(), []);
    let (mut v, task) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, enc::RAW]).await;
    v.request(false).await;
    v.update().await;
    let list = server.clients();
    assert_eq!(list.len(), 1);
    let info = &list[0];
    assert_eq!(info.encryption, "none");
    assert_eq!((info.user.as_deref(), info.view_only), (None, false));
    assert!(info.updates >= 1 && info.encoding == enc::ZRLE, "{info:?}");
    // The counters move with traffic (they are refreshed each time the task
    // wakes, so poke it first).
    v.key(0x61).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let later = &server.clients()[0];
    assert!(later.bytes_out > 0 && later.bytes_in > 0, "{later:?}");

    // View-only can be switched on and off while connected.
    let id = info.id;
    assert!(server.set_view_only(id, true));
    assert!(server.clients()[0].view_only);
    // The key held down above is let go when the client becomes view only.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(input.seen(), ["key 61 true", "key 61 false"]);
    input.0.0.lock().unwrap().clear();
    v.key(0x62).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(input.seen().len(), 0, "input is dropped now");
    assert!(server.set_view_only(id, false));
    v.key(0x63).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(input.seen(), ["key 63 true"]);
    assert!(!server.set_view_only(9999, true), "no such client");

    // Disconnecting ends the session and removes it from the list.
    assert!(server.disconnect(id));
    let ended = timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(ended.is_err());
    assert_eq!(server.clients(), []);
    assert!(!server.disconnect(id));
}

#[tokio::test]
async fn the_preview_is_a_scaled_copy_of_the_screen() {
    let server = Server::new(
        TestPattern::new(400, 200),
        NoInput,
        options(),
        Settings::default(),
    )
    .unwrap();
    let p = server.preview(100);
    assert_eq!((p.width, p.height), (100, 50));
    assert_eq!(p.rgba.len(), 100 * 50 * 4);
    // The top-left pixel is the pattern's, in R, G, B order.
    let [b, g, r, _] = TestPattern::background(0, 0);
    assert_eq!(&p.rgba[..4], [r, g, b, 255]);
    // A small screen is not enlarged, and a request of 0 still gives a pixel.
    let small = Server::new(
        TestPattern::new(20, 10),
        NoInput,
        options(),
        Settings::default(),
    )
    .unwrap();
    assert_eq!(
        (small.preview(500).width, small.preview(500).height),
        (20, 10)
    );
    assert!(small.preview(0).width >= 1);
}

/// Answer every fence the server has asked for so far (taken from `other`).
async fn answer_fences(v: &mut Viewer) {
    v.quiet(Duration::from_millis(100)).await;
    let asked: Vec<_> = v
        .other
        .drain(..)
        .filter_map(|e| match e {
            ClientConnEvent::Fence { flags, payload } if flags & fence::REQUEST != 0 => {
                Some((flags & !fence::REQUEST, payload))
            }
            _ => None,
        })
        .collect();
    for (flags, payload) in asked {
        v.session
            .send(&ClientMessage::Fence { flags, payload })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_client_that_has_not_confirmed_earlier_updates_is_not_sent_more() {
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
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, pseudo::FENCE]).await;
    v.request(false).await;
    v.update().await; // the first update: one fence in flight
    let change = |n: u8| {
        let mut p = plan.0.lock().unwrap();
        p.shade = n;
        p.damage = vec![Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        }];
    };
    change(10);
    v.request(true).await;
    v.update().await; // the second: two in flight
    change(20);
    v.request(true).await;
    assert!(
        timeout(Duration::from_millis(400), v.update())
            .await
            .is_err(),
        "two updates are unconfirmed, so the third waits"
    );
    // Confirm them: the waiting update arrives, carrying everything since.
    answer_fences(&mut v).await;
    let rects = v.update().await;
    assert_eq!(rects.len(), 1);
    v.matches(
        Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        },
        |_, _| [20, 20, 20, 0],
    );
    // The round trip was measured.
    assert!(server.clients()[0].rtt.is_some(), "{:?}", server.clients());
}

#[tokio::test]
async fn updates_are_not_paced_for_clients_without_fence() {
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
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    v.update().await;
    for n in 1..=4u8 {
        {
            let mut p = plan.0.lock().unwrap();
            p.shade = n * 10;
            p.damage = vec![Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            }];
        }
        v.request(true).await;
        v.update().await;
    }
    assert_eq!(server.clients()[0].rtt, None);
}

#[tokio::test]
async fn a_client_that_never_answers_fences_is_not_stalled_for_ever() {
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
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, pseudo::FENCE]).await;
    for n in 0..3u8 {
        {
            let mut p = plan.0.lock().unwrap();
            p.shade = n;
            p.damage = vec![Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            }];
        }
        v.request(n == 0).await;
        // The third waits until the first fence is given up on (5 s).
        timeout(Duration::from_secs(9), v.update())
            .await
            .expect("the server stopped sending");
    }
}

/// A screen of 160 x 120 showing lines of text-like content that scrolls: the
/// line shown at row `r` is `top + r`.
#[derive(Debug, Clone, Default)]
struct Scroll(Arc<Mutex<usize>>);

struct Scroller {
    state: Scroll,
    painted: Option<usize>,
}

#[allow(clippy::cast_possible_truncation)] // a pattern: wrapping is the point
fn line_pixel(line: usize, x: u16) -> [u8; 4] {
    let v = line * 37 + usize::from(x) * 5;
    [v as u8, (v >> 8) as u8 ^ 0x33, (v / 7) as u8, 0]
}

impl Source for Scroller {
    fn size(&self) -> (u16, u16) {
        (160, 120)
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let top = *self.state.0.lock().unwrap();
        if self.painted == Some(top) {
            return Ok(Capture::default());
        }
        let mut data = Vec::new();
        for r in 0..120usize {
            for x in 0..160u16 {
                data.extend_from_slice(&line_pixel(top + r, x));
            }
        }
        let whole = Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 120,
        };
        fb.write_bgrx(whole, &data).unwrap();
        self.painted = Some(top);
        Ok(Capture {
            damage: vec![whole],
            ..Capture::default()
        })
    }
}

fn scrolling_server() -> (Arc<Server>, Scroll) {
    let state = Scroll::default();
    let source = Scroller {
        state: state.clone(),
        painted: None,
    };
    let server = Server::new(
        source,
        NoInput,
        options(),
        Settings {
            fps: 100,
            view_only: false,
        },
    )
    .unwrap();
    (Arc::new(server), state)
}

fn shows_top(v: &Viewer, top: usize) {
    v.matches(whole(160, 120), |x, y| line_pixel(top + usize::from(y), x));
}

#[tokio::test]
async fn scrolling_is_sent_as_a_copy_to_clients_that_can_use_it() {
    let (server, scroll) = scrolling_server();
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, enc::COPY_RECT]).await;
    v.request(false).await;
    v.update().await;
    shows_top(&v, 0);
    *scroll.0.lock().unwrap() = 20;
    v.request(true).await;
    let rects = v.update().await;
    let copies: Vec<_> = rects.iter().filter(|r| r.1 == enc::COPY_RECT).collect();
    assert_eq!(copies.len(), 1, "{rects:?}");
    assert_eq!((copies[0].0.y, copies[0].0.height), (0, 100));
    // Only the 20 new rows cost pixels.
    let pixel_area: u32 = rects
        .iter()
        .filter(|r| r.1 != enc::COPY_RECT)
        .map(|r| u32::from(r.0.width) * u32::from(r.0.height))
        .sum();
    assert_eq!(pixel_area, 160 * 20, "{rects:?}");
    shows_top(&v, 20);
}

/// A pixel of a picture whose every column differs (scrolled sideways).
#[allow(clippy::cast_possible_truncation)] // colour bytes
fn column_pixel(col: usize, y: u16) -> [u8; 4] {
    let v = col * 31 + usize::from(y) * 7;
    [v as u8, (v >> 8) as u8 ^ 0x5A, (v / 3) as u8, 255]
}

/// Paints the picture shifted `left` columns.
#[derive(Debug)]
struct SideScroller {
    state: Scroll,
    painted: Option<usize>,
}

impl Source for SideScroller {
    fn size(&self) -> (u16, u16) {
        (160, 120)
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let left = *self.state.0.lock().unwrap();
        if self.painted == Some(left) {
            return Ok(Capture::default());
        }
        let mut data = Vec::new();
        for y in 0..120u16 {
            for x in 0..160usize {
                data.extend_from_slice(&column_pixel(left + x, y));
            }
        }
        let whole = Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 120,
        };
        fb.write_bgrx(whole, &data).unwrap();
        self.painted = Some(left);
        Ok(Capture {
            damage: vec![whole],
            ..Capture::default()
        })
    }
}

#[tokio::test]
async fn sideways_scrolling_is_sent_as_a_copy_too() {
    let state = Scroll::default();
    let server = Arc::new(
        Server::new(
            SideScroller {
                state: state.clone(),
                painted: None,
            },
            NoInput,
            options(),
            Settings {
                fps: 100,
                view_only: false,
            },
        )
        .unwrap(),
    );
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, enc::COPY_RECT]).await;
    v.request(false).await;
    v.update().await;
    *state.0.lock().unwrap() = 20;
    v.request(true).await;
    let rects = v.update().await;
    let copies: Vec<_> = rects.iter().filter(|r| r.1 == enc::COPY_RECT).collect();
    assert_eq!(copies.len(), 1, "{rects:?}");
    assert_eq!((copies[0].0.x, copies[0].0.width), (0, 140), "{rects:?}");
    // Only the 20 new columns cost pixels.
    let pixel_area: u32 = rects
        .iter()
        .filter(|r| r.1 != enc::COPY_RECT)
        .map(|r| u32::from(r.0.width) * u32::from(r.0.height))
        .sum();
    assert_eq!(pixel_area, 20 * 120, "{rects:?}");
    v.matches(whole(160, 120), |x, y| column_pixel(20 + usize::from(x), y));
}

#[tokio::test]
async fn clients_without_copy_rect_get_pixels_and_the_picture_is_right_either_way() {
    let (server, scroll) = scrolling_server();
    let (mut plain, _t1) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    plain.request(false).await;
    plain.update().await;
    *scroll.0.lock().unwrap() = 25;
    plain.request(true).await;
    let rects = plain.update().await;
    assert!(rects.iter().all(|r| r.1 != enc::COPY_RECT), "{rects:?}");
    shows_top(&plain, 25);
}

#[tokio::test]
async fn several_scrolls_before_the_client_asks_still_give_the_right_picture() {
    let (server, scroll) = scrolling_server();
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, &[enc::ZRLE, enc::COPY_RECT]).await;
    v.request(false).await;
    v.update().await;
    // Scroll three times while the client has no request outstanding. Some of
    // these copies cannot be used (their source is still waiting for pixels),
    // and the result has to be right anyway.
    for top in [15, 40, 41, 90] {
        *scroll.0.lock().unwrap() = top;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    v.request(true).await;
    v.update().await;
    // More may be pending; ask until the picture settles.
    for _ in 0..3 {
        v.request(true).await;
        let _ = timeout(Duration::from_millis(300), v.update()).await;
    }
    shows_top(&v, 90);
}

#[tokio::test]
async fn connections_beyond_the_limit_are_dropped_at_once_and_slots_come_back() {
    let mut server = Server::new(
        TestPattern::new(64, 64),
        NoInput,
        options(),
        Settings::default(),
    )
    .unwrap();
    server.set_max_connections(2);
    let server = Arc::new(server);
    // Two connections that never speak occupy the two slots (handshake pending).
    let mut idle = Vec::new();
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let (near, far) = duplex(1 << 16);
        let s = Arc::clone(&server);
        tasks.push(tokio::spawn(async move { s.serve_connection(far).await }));
        idle.push(near);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    // The third is refused without any handshake.
    let (_near, far) = duplex(1 << 16);
    let err = timeout(Duration::from_secs(1), server.serve_connection(far))
        .await
        .expect("refused at once")
        .unwrap_err();
    assert!(err.to_string().contains("too many connections"), "{err}");
    // Closing one frees its slot.
    drop(idle.pop());
    assert!(
        timeout(Duration::from_secs(2), tasks.pop().unwrap())
            .await
            .is_ok()
    );
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    v.update().await;
}

/// A server that holds clients for approval.
fn approving(input: Recording) -> Server {
    let server = Server::new(
        TestPattern::new(64, 64),
        input,
        options(),
        Settings::default(),
    )
    .unwrap();
    server.set_require_approval(true);
    server
}

/// Wait for a client to appear in the pending list and answer it.
async fn answer_when_waiting(server: &Arc<Server>, decision: rfb_server::Decision) {
    for _ in 0..200 {
        if let Some(p) = server.pending().first() {
            assert!(server.decide(p.id, decision));
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nobody was waiting for approval");
}

#[tokio::test]
async fn an_approved_client_gets_pictures_and_input_only_after_the_answer() {
    let input = Recording::default();
    let server = Arc::new(approving(input.clone()));
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    v.key(0x61).await;
    v.quiet(Duration::from_millis(500)).await;
    assert_eq!(server.pending().len(), 1, "it waits");
    assert!(
        !v.other
            .iter()
            .any(|e| matches!(e, ClientConnEvent::UpdateBegin { .. })),
        "no picture before approval"
    );
    assert!(server.clients().is_empty(), "not listed as a client yet");
    let s = Arc::clone(&server);
    tokio::spawn(async move { answer_when_waiting(&s, rfb_server::Decision::Allow).await });
    let rects = v.update().await;
    assert_ne!(rects.len(), 0, "a picture follows the approval");
    assert_eq!(server.pending(), []);
    assert_eq!(server.clients().len(), 1);
    assert!(
        input.seen().contains(&"key 61 true".to_owned()),
        "what it sent while waiting is handled after: {:?}",
        input.seen()
    );
}

#[tokio::test]
async fn view_only_approval_ignores_input_and_a_denial_closes_the_connection() {
    let input = Recording::default();
    let server = Arc::new(approving(input.clone()));
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    let s = Arc::clone(&server);
    tokio::spawn(async move { answer_when_waiting(&s, rfb_server::Decision::ViewOnly).await });
    v.request(false).await;
    v.update().await;
    v.key(0x62).await;
    v.quiet(Duration::from_millis(300)).await;
    assert!(input.seen().is_empty(), "{:?}", input.seen());
    assert!(server.clients()[0].view_only);

    let (mut denied, task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    let s = Arc::clone(&server);
    tokio::spawn(async move { answer_when_waiting(&s, rfb_server::Decision::Deny).await });
    denied.request(false).await;
    let end = timeout(Duration::from_secs(5), denied.session.next_event())
        .await
        .expect("the server answered");
    assert!(end.is_err(), "a denied client is disconnected");
    assert!(task.await.unwrap().is_err());
    assert_eq!(server.clients().len(), 1, "only the allowed one is left");
}

#[tokio::test]
async fn an_unanswered_client_is_turned_away_after_the_timeout() {
    let mut server = approving(Recording::default());
    server.set_approval_timeout(Duration::from_millis(300));
    let server = Arc::new(server);
    let (mut v, task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    let end = timeout(Duration::from_secs(5), v.session.next_event())
        .await
        .expect("the server answered");
    assert!(end.is_err());
    assert!(task.await.unwrap().is_err());
    assert_eq!(server.pending(), []);
    assert!(
        !server.decide(1, rfb_server::Decision::Allow),
        "nobody left to answer"
    );
}

#[tokio::test]
async fn view_only_for_everyone_and_approval_can_be_switched_while_running() {
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
    let (mut v, _task) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    v.request(false).await;
    v.update().await;
    v.key(0x61).await;
    v.quiet(Duration::from_millis(200)).await;
    assert_eq!(input.seen(), ["key 61 true"]);
    server.set_view_only_for_all(true);
    assert!(
        server.clients()[0].view_only,
        "already connected clients too"
    );
    // The key held down above is let go.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(input.seen(), ["key 61 true", "key 61 false"]);
    input.0.0.lock().unwrap().clear();
    v.key(0x62).await;
    v.quiet(Duration::from_millis(200)).await;
    assert_eq!(input.seen().len(), 0, "{:?}", input.seen());
    // New clients start view only as well, and approval applies to the next one.
    server.set_require_approval(true);
    let (_waiting, _t) = connect(&server, PixelFormat::RGB888, ZRLE).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.pending().len(), 1);
    server.set_require_approval(false);
    server.set_view_only_for_all(false);
}
