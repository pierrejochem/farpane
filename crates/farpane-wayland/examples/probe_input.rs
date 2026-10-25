//! Live check of the Wayland backend's buttons, wheel and typing. The target
//! is a window of its own (through `XWayland`): the probe moves the pointer
//! onto it and stops unless the window sees the pointer arrive, so no click
//! or key is sent anywhere else.
//!
//! `cargo run -p farpane-wayland --example probe_input` (answer the portal
//! dialog; keep the mouse still and leave the middle of the screen free).

#[cfg(target_os = "linux")]
mod live {
    use std::{
        thread::sleep,
        time::{Duration, Instant},
    };

    use farpane_wayland::{Config, open};
    use rfb_server::{Input, Source};
    use x11rb::{
        COPY_DEPTH_FROM_PARENT, CURRENT_TIME,
        connection::Connection,
        protocol::{
            Event,
            xproto::{ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, WindowClass},
        },
        rust_connection::RustConnection,
    };

    type Conn = RustConnection;

    /// Events seen so far, drained from the connection.
    fn drain(conn: &Conn, win: u32, seen: &mut Vec<Event>) {
        while let Some(ev) = conn.poll_for_event().unwrap() {
            let ours = match &ev {
                Event::ButtonPress(e) | Event::ButtonRelease(e) => e.event == win,
                Event::KeyPress(e) | Event::KeyRelease(e) => e.event == win,
                Event::MotionNotify(e) => e.event == win,
                Event::EnterNotify(e) => e.event == win,
                _ => false,
            };
            if ours {
                seen.push(ev);
            }
        }
    }

    fn wait(conn: &Conn, win: u32, seen: &mut Vec<Event>, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            drain(conn, win, seen);
            sleep(Duration::from_millis(20));
        }
    }

    #[allow(clippy::too_many_lines)] // a linear script
    pub fn main() {
        let token = std::env::temp_dir().join("farpane-probe-token");
        let (source, input) = open(&Config {
            restore_token_file: Some(token),
            parent_window: None,
        })
        .expect("open");
        let (w, h) = source.size();
        println!("sharing {w}x{h}");

        let (conn, num) = x11rb::connect(None).expect("X display");
        let root = conn.setup().roots[num].root;
        let win = conn.generate_id().unwrap();
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            500,
            400,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(
                EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::ENTER_WINDOW,
            ),
        )
        .unwrap();
        conn.map_window(win).unwrap();
        conn.flush().unwrap();
        sleep(Duration::from_millis(800));

        let mut seen = Vec::new();
        // Calibrate: where is the stream in the desktop?
        let at = || {
            let p = conn.query_pointer(root).unwrap().reply().unwrap();
            (i32::from(p.root_x), i32::from(p.root_y))
        };
        input.pointer(w / 2, h / 2, 0);
        sleep(Duration::from_millis(500));
        let got = at();
        let offset = (got.0 - i32::from(w / 2), got.1 - i32::from(h / 2));
        println!("stream offset {offset:?}");
        drain(&conn, win, &mut seen);
        seen.clear();

        // Where is our window on the desktop?
        let pos = conn
            .translate_coordinates(win, root, 0, 0)
            .unwrap()
            .reply()
            .unwrap();
        let centre = (i32::from(pos.dst_x) + 250, i32::from(pos.dst_y) + 200);
        let target = (centre.0 - offset.0, centre.1 - offset.1);
        println!(
            "window at ({}, {}), centre on stream {target:?}",
            pos.dst_x, pos.dst_y
        );
        let (Ok(tx), Ok(ty)) = (u16::try_from(target.0), u16::try_from(target.1)) else {
            println!("window is off the shared stream: stop");
            return;
        };
        if tx >= w || ty >= h {
            println!("window is off the shared stream: stop");
            return;
        }

        // Move in two steps so the window sees an Enter or Motion.
        input.pointer(tx.saturating_sub(5), ty.saturating_sub(5), 0);
        sleep(Duration::from_millis(300));
        input.pointer(tx, ty, 0);
        wait(&conn, win, &mut seen, 700);
        let arrived = seen
            .iter()
            .any(|e| matches!(e, Event::EnterNotify(_) | Event::MotionNotify(_)));
        if !arrived {
            println!("the window never saw the pointer (covered?): stop, nothing clicked");
            return;
        }
        println!("pointer reached the window");
        seen.clear();

        // Buttons: bit 0 left, 1 middle, 2 right; wheel: bit 3 up, bit 4 down.
        for (name, mask, button) in [
            ("left", 1u8, 1u8),
            ("middle", 2, 2),
            ("right", 4, 3),
            ("wheel up", 8, 4),
            ("wheel down", 16, 5),
        ] {
            seen.clear();
            input.pointer(tx, ty, mask);
            sleep(Duration::from_millis(60));
            input.pointer(tx, ty, 0);
            wait(&conn, win, &mut seen, 500);
            let press = seen
                .iter()
                .filter(|e| matches!(e, Event::ButtonPress(b) if b.detail == button))
                .count();
            let release = seen
                .iter()
                .filter(|e| matches!(e, Event::ButtonRelease(b) if b.detail == button))
                .count();
            println!(
                "{name}: {press} press, {release} release {}",
                if press == 1 { "ok" } else { "MISSING" }
            );
        }

        // Typing needs keyboard focus; the left click should have given it.
        let _ = conn.set_input_focus(InputFocus::POINTER_ROOT, win, CURRENT_TIME);
        conn.flush().unwrap();
        sleep(Duration::from_millis(200));
        seen.clear();
        let text = "farpane";
        for c in text.chars() {
            let keysym = c as u32;
            input.key(keysym, true);
            input.key(keysym, false);
            sleep(Duration::from_millis(40));
        }
        // Shift held: a capital, then an umlaut the layout may not have.
        input.key(0xFFE1, true);
        input.key(u32::from('A'), true);
        input.key(u32::from('A'), false);
        input.key(0xFFE1, false);
        input.key(0x00E4, true);
        input.key(0x00E4, false);
        wait(&conn, win, &mut seen, 800);

        let m = conn
            .get_keyboard_mapping(
                conn.setup().min_keycode,
                conn.setup().max_keycode - conn.setup().min_keycode + 1,
            )
            .unwrap()
            .reply()
            .unwrap();
        let per = usize::from(m.keysyms_per_keycode);
        let min = usize::from(conn.setup().min_keycode);
        let mut typed = String::new();
        let mut presses = 0;
        for e in &seen {
            if let Event::KeyPress(k) = e {
                presses += 1;
                let row = (usize::from(k.detail) - min) * per;
                let col = usize::from(k.state.bits() & 1);
                let sym = m.keysyms.get(row + col).copied().unwrap_or(0);
                typed.push(char::from_u32(sym).filter(|c| *c != '\0').unwrap_or('?'));
            }
        }
        println!("{presses} key presses, window read {typed:?}");
        println!(
            "typing {}",
            if typed.starts_with(text) {
                "ok"
            } else {
                "MISSING"
            }
        );
        println!("portal errors: {:?}", input.first_error());
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    live::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("the Wayland backend is only available on Linux");
}
