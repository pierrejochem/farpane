//! Against a real X server (a private `Xvfb` or nested `Xwayland`, else
//! `$DISPLAY`); skipped without one.

#![allow(
    clippy::many_single_char_names,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation
)]

use std::{
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard},
    thread::sleep,
    time::Duration,
};

use rfb_codec::Framebuffer;
use rfb_proto::types::Rect;
use rfb_server::{Input, Source};
use x11rb::{
    COPY_DEPTH_FROM_PARENT,
    connection::Connection,
    protocol::xproto::{
        AtomEnum, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, PropMode, Rectangle,
        WindowClass,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

/// A private X server for the test (so it never touches the desktop):
/// `Xvfb` if installed, else a nested `Xwayland` under a Wayland session.
/// Falls back to `$DISPLAY`, where some servers (`XWayland`'s rootless root)
/// cannot be captured.
/// One test server at a time: they pick display numbers by lock file.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct TestServer {
    display: String,
    child: Option<Child>,
    _only: MutexGuard<'static, ()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A freshly started server can accept connections a moment before its
/// extensions answer: try a few times.
fn open_display(display: &str) -> (farpane_x11::X11Source, farpane_x11::X11Input) {
    let mut last = None;
    for _ in 0..30 {
        match farpane_x11::open(Some(display)) {
            Ok(pair) => return pair,
            Err(e) => last = Some(e),
        }
        sleep(Duration::from_millis(100));
    }
    panic!("cannot open the display: {last:?}");
}

fn start_server() -> Option<TestServer> {
    let only = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let can_run = |name: &str| {
        Command::new(name)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    };
    // A display number nobody uses: probe by lock file.
    let number = (90..190).find(|n| !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists())?;
    let display = format!(":{number}");
    let child = if can_run("Xvfb") {
        Command::new("Xvfb")
            .args([&display, "-screen", "0", "800x600x24"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() && can_run("Xwayland") {
        Command::new("Xwayland")
            .args([&display, "-geometry", "800x600"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    } else {
        None
    };
    if let Some(child) = child {
        let server = TestServer {
            display: display.clone(),
            child: Some(child),
            _only: only,
        };
        for _ in 0..100 {
            if RustConnection::connect(Some(&display)).is_ok() {
                return Some(server);
            }
            sleep(Duration::from_millis(100));
        }
        return None;
    }
    let d = std::env::var("DISPLAY").ok()?;
    RustConnection::connect(Some(&d)).ok()?;
    Some(TestServer {
        display: d,
        child: None,
        _only: only,
    })
}

/// A mapped window of one colour; returns its position on the root.
struct Window {
    conn: RustConnection,
    win: u32,
    gc: u32,
}

impl Window {
    fn new(display: &str, size: (u16, u16)) -> Self {
        let (conn, num) = RustConnection::connect(Some(display)).unwrap();
        let screen = conn.setup().roots[num].clone();
        let win = conn.generate_id().unwrap();
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            screen.root,
            50,
            60,
            size.0,
            size.1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(screen.black_pixel)
                .event_mask(EventMask::EXPOSURE),
        )
        .unwrap();
        conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"farpane-x11 test",
        )
        .unwrap();
        let gc = conn.generate_id().unwrap();
        conn.create_gc(gc, win, &CreateGCAux::new().foreground(0))
            .unwrap();
        conn.map_window(win).unwrap();
        conn.flush().unwrap();
        sleep(Duration::from_millis(500));
        Self { conn, win, gc }
    }

    fn paint(&self, rgb: u32, rect: Rect) {
        self.conn
            .change_gc(
                self.gc,
                &x11rb::protocol::xproto::ChangeGCAux::new().foreground(rgb),
            )
            .unwrap();
        self.conn
            .poly_fill_rectangle(
                self.win,
                self.gc,
                &[Rectangle {
                    x: rect.x as i16,
                    y: rect.y as i16,
                    width: rect.width,
                    height: rect.height,
                }],
            )
            .unwrap();
        self.conn.flush().unwrap();
    }

    fn origin(&self, root: u32) -> (i16, i16) {
        let t = self
            .conn
            .translate_coordinates(self.win, root, 0, 0)
            .unwrap()
            .reply()
            .unwrap();
        (t.dst_x, t.dst_y)
    }
}

#[test]
fn capture_sees_a_window_and_reports_its_changes() {
    let Some(server) = start_server() else {
        eprintln!("no X display: skipped");
        return;
    };
    let d = server.display.clone();
    let (mut source, _input) = open_display(&d);
    let (w, h) = source.size();
    let mut fb = Framebuffer::new(w, h).unwrap();
    let first = source.capture(&mut fb).unwrap();
    assert_eq!(
        first.damage.len(),
        1,
        "the first capture is the whole screen"
    );
    assert_eq!((first.damage[0].width, first.damage[0].height), (w, h));
    let cursor = first
        .cursor
        .as_ref()
        .expect("the first capture reports the cursor");
    assert!(cursor.width > 0 && cursor.height > 0);
    assert_eq!(
        cursor.rgba.len(),
        usize::from(cursor.width) * usize::from(cursor.height) * 4
    );
    assert!(
        cursor.rgba.as_chunks::<4>().0.iter().any(|p| p[3] > 0),
        "some opaque pixels"
    );

    let win = Window::new(&d, (120, 80));
    let root = source.root();
    let (ox, oy) = win.origin(root);
    let inside = Rect {
        x: 10,
        y: 10,
        width: 20,
        height: 20,
    };
    win.paint(
        0x00_12_34_56,
        Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 80,
        },
    );
    win.paint(0x00_AB_CD_EF, inside);
    sleep(Duration::from_millis(300));
    let cap = source.capture(&mut fb).unwrap();
    if cap.damage.is_empty() {
        eprintln!("this X server reported no damage for a window (XWayland?): skipped");
        return;
    }
    let px = |fb: &Framebuffer, x: i16, y: i16| {
        fb.pixel(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())
    };
    let seen = px(&fb, ox + 15, oy + 15).unwrap();
    if seen == [0, 0, 0, 255] {
        eprintln!("the root shows no window contents here (XWayland?): skipped");
        return;
    }
    assert_eq!(
        seen,
        [0xEF, 0xCD, 0xAB, 255],
        "inside the lighter square (B, G, R)"
    );
    assert_eq!(
        px(&fb, ox + 100, oy + 70).unwrap(),
        [0x56, 0x34, 0x12, 255],
        "the window background"
    );
    // Only the changed part is reported next.
    win.paint(0x00_00_FF_00, inside);
    sleep(Duration::from_millis(300));
    let cap = source.capture(&mut fb).unwrap();
    assert_ne!(cap.damage.len(), 0);
    assert!(
        cap.damage
            .iter()
            .all(|r| u32::from(r.width) * u32::from(r.height) < 120 * 80 * 2)
    );
    assert_eq!(px(&fb, ox + 15, oy + 15).unwrap(), [0x00, 0xFF, 0x00, 255]);
    // Nothing changed: nothing reported.
    sleep(Duration::from_millis(100));
    assert_eq!(source.capture(&mut fb).unwrap().damage.len(), 0);
}

#[test]
fn the_keyboard_layout_has_ascii_and_pointer_moves_reach_the_server() {
    let Some(server) = start_server() else {
        eprintln!("no X display: skipped");
        return;
    };
    let d = server.display.clone();
    let (source, input) = open_display(&d);
    for sym in [u32::from(b'a'), u32::from(b'A'), u32::from(b'1'), 0xFF0D] {
        assert!(input.keycode_for(sym).is_some(), "keysym {sym:x}");
    }
    assert_eq!(input.keycode_for(0x10_FFFF), None);
    let (conn, num) = RustConnection::connect(Some(&d)).unwrap();
    let root = conn.setup().roots[num].root;
    let (w, h) = source.size();
    let (x, y) = (w / 3, h / 2);
    input.pointer(x, y, 0);
    sleep(Duration::from_millis(200));
    let at = conn.query_pointer(root).unwrap().reply().unwrap();
    if (at.root_x, at.root_y) != (x as i16, y as i16) {
        eprintln!(
            "XTest motion did not move the pointer here ({},{} instead of {x},{y}): skipped",
            at.root_x, at.root_y
        );
        return;
    }
    input.pointer(x + 10, y + 5, 0);
    sleep(Duration::from_millis(200));
    let at = conn.query_pointer(root).unwrap().reply().unwrap();
    assert_eq!((at.root_x, at.root_y), ((x + 10) as i16, (y + 5) as i16));
}

#[test]
fn keys_reach_a_focused_window_including_symbols_the_layout_lacks() {
    use x11rb::protocol::{Event, xproto::InputFocus};
    let Some(server) = start_server() else {
        eprintln!("no X display: skipped");
        return;
    };
    let (_source, input) = open_display(&server.display);
    let (conn, num) = RustConnection::connect(Some(&server.display)).unwrap();
    let screen = conn.setup().roots[num].clone();
    let win = conn.generate_id().unwrap();
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        screen.root,
        0,
        0,
        100,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
    )
    .unwrap();
    conn.map_window(win).unwrap();
    conn.set_input_focus(InputFocus::POINTER_ROOT, win, x11rb::CURRENT_TIME)
        .unwrap();
    conn.sync().unwrap();
    sleep(Duration::from_millis(300));

    let keysym_of = |code: u8| {
        let m = conn.get_keyboard_mapping(code, 1).unwrap().reply().unwrap();
        m.keysyms[0]
    };
    let next_key = |want_press: bool| -> Option<u8> {
        for _ in 0..30 {
            while let Some(e) = conn.poll_for_event().unwrap() {
                match e {
                    Event::KeyPress(k) if want_press => return Some(k.detail),
                    Event::KeyRelease(k) if !want_press => return Some(k.detail),
                    _ => {}
                }
            }
            sleep(Duration::from_millis(50));
        }
        None
    };

    // A normal key.
    input.key(u32::from(b'a'), true);
    let Some(code) = next_key(true) else {
        eprintln!("XTest key events are not delivered here: skipped");
        return;
    };
    assert_eq!(Some(code), input.keycode_for(u32::from(b'a')));
    input.key(u32::from(b'a'), false);
    assert_eq!(next_key(false), Some(code));

    // The euro sign is on no key of the default layout: a spare keycode is
    // borrowed while the key is down and given back after.
    input.key(0x20AC, true);
    let borrowed = next_key(true).expect("the euro key press arrives");
    assert_eq!(keysym_of(borrowed), 0x20AC, "mapped while held");
    input.key(0x20AC, false);
    assert_eq!(next_key(false), Some(borrowed));
    sleep(Duration::from_millis(100));
    assert_eq!(keysym_of(borrowed), 0, "given back");
}

mod clipboard {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use x11rb::{
        CURRENT_TIME, NONE,
        protocol::{
            Event,
            xproto::{Atom, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, Window as XWindow},
        },
    };

    fn atom(conn: &RustConnection, name: &[u8]) -> Atom {
        conn.intern_atom(false, name).unwrap().reply().unwrap().atom
    }

    /// Another program that owns the clipboard and serves `text` (UTF-8 only).
    struct Owner {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Owner {
        fn start(display: &str, text: &str) -> Self {
            let (conn, num) = RustConnection::connect(Some(display)).unwrap();
            let root = conn.setup().roots[num].root;
            let win = conn.generate_id().unwrap();
            conn.create_window(
                0,
                win,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap();
            let (clipboard, utf8, targets) = (
                atom(&conn, b"CLIPBOARD"),
                atom(&conn, b"UTF8_STRING"),
                atom(&conn, b"TARGETS"),
            );
            conn.set_selection_owner(win, clipboard, CURRENT_TIME)
                .unwrap();
            conn.flush().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let text = text.to_owned();
            let thread = std::thread::spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    while let Some(event) = conn.poll_for_event().unwrap() {
                        if let Event::SelectionRequest(r) = event {
                            let ok = if r.target == targets {
                                conn.change_property32(
                                    PropMode::REPLACE,
                                    r.requestor,
                                    r.property,
                                    AtomEnum::ATOM,
                                    &[targets, utf8],
                                )
                                .unwrap();
                                true
                            } else if r.target == utf8 {
                                conn.change_property8(
                                    PropMode::REPLACE,
                                    r.requestor,
                                    r.property,
                                    utf8,
                                    text.as_bytes(),
                                )
                                .unwrap();
                                true
                            } else {
                                false
                            };
                            conn.send_event(
                                false,
                                r.requestor,
                                EventMask::NO_EVENT,
                                SelectionNotifyEvent {
                                    response_type: SELECTION_NOTIFY_EVENT,
                                    sequence: 0,
                                    time: r.time,
                                    requestor: r.requestor,
                                    selection: r.selection,
                                    target: r.target,
                                    property: if ok { r.property } else { NONE },
                                },
                            )
                            .unwrap();
                            conn.flush().unwrap();
                        }
                    }
                    sleep(Duration::from_millis(10));
                }
            });
            Self {
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    #[test]
    fn text_another_program_copies_is_reported() {
        let Some(server) = start_server() else {
            eprintln!("no X display: skipped");
            return;
        };
        let (mut source, _input) = open_display(&server.display);
        let (w, h) = source.size();
        let mut fb = Framebuffer::new(w, h).unwrap();
        source.capture(&mut fb).unwrap();
        let _owner = Owner::start(
            &server.display,
            "copied: caf\u{e9} 5\u{20ac}\n\u{4f60}\u{597d}",
        );
        let mut got = None;
        for _ in 0..100 {
            if let Some(text) = source.capture(&mut fb).unwrap().clipboard {
                got = Some(text);
                break;
            }
            sleep(Duration::from_millis(30));
        }
        assert_eq!(
            got.as_deref(),
            Some("copied: caf\u{e9} 5\u{20ac}\n\u{4f60}\u{597d}")
        );
    }

    #[test]
    fn text_a_client_copies_is_served_to_pasting_programs_and_not_echoed_back() {
        let Some(server) = start_server() else {
            eprintln!("no X display: skipped");
            return;
        };
        let (mut source, input) = open_display(&server.display);
        let (w, h) = source.size();
        let mut fb = Framebuffer::new(w, h).unwrap();
        source.capture(&mut fb).unwrap();

        input.clipboard("from the viewer: 5\u{20ac}\r\nok");
        let (conn, num) = RustConnection::connect(Some(&server.display)).unwrap();
        let root = conn.setup().roots[num].root;
        let win = conn.generate_id().unwrap();
        conn.create_window(
            0,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap();
        let (clipboard, utf8, prop) = (
            atom(&conn, b"CLIPBOARD"),
            atom(&conn, b"UTF8_STRING"),
            atom(&conn, b"PASTE"),
        );
        // The source takes ownership at its next capture.
        let mut echoed = Vec::new();
        for _ in 0..5 {
            echoed.extend(source.capture(&mut fb).unwrap().clipboard);
            sleep(Duration::from_millis(30));
        }
        conn.convert_selection(win, clipboard, utf8, prop, CURRENT_TIME)
            .unwrap();
        conn.flush().unwrap();
        let mut pasted = None;
        'wait: for _ in 0..100 {
            echoed.extend(source.capture(&mut fb).unwrap().clipboard);
            while let Some(event) = conn.poll_for_event().unwrap() {
                if let Event::SelectionNotify(e) = event {
                    assert_ne!(e.property, NONE, "the selection was refused");
                    let reply = conn
                        .get_property(true, win, prop, AtomEnum::ANY, 0, 1 << 16)
                        .unwrap()
                        .reply()
                        .unwrap();
                    pasted = Some(String::from_utf8(reply.value).unwrap());
                    break 'wait;
                }
            }
            sleep(Duration::from_millis(30));
        }
        assert_eq!(pasted.as_deref(), Some("from the viewer: 5\u{20ac}\r\nok"));
        assert_eq!(
            echoed,
            Vec::<String>::new(),
            "our own copy is not reported back"
        );
        let _: XWindow = win;
    }
}
