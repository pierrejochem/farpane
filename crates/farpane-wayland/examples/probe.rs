//! Live check of the Wayland backend's input: moves the pointer to three
//! places (no clicks, no keys except a Shift tap), asks the X server which
//! `XWayland` reports where the pointer is, and puts the pointer back.
//!
//! `cargo run -p farpane-wayland --example probe` (answer the portal dialog).

#[cfg(target_os = "linux")]
mod live {
    use std::{thread::sleep, time::Duration};

    use farpane_wayland::{Config, open};
    use rfb_server::{Input, Source};
    use x11rb::{connection::Connection, protocol::xproto::ConnectionExt as _};

    pub fn main() {
        let token = std::env::temp_dir().join("farpane-probe-token");
        let (source, input) = open(&Config {
            restore_token_file: Some(token),
        })
        .expect("open");
        let (w, h) = source.size();
        println!("sharing {w}x{h}");
        let (conn, num) = x11rb::connect(None).expect("X display");
        let root = conn.setup().roots[num].root;
        let at = |c: &x11rb::rust_connection::RustConnection| {
            let p = c.query_pointer(root).unwrap().reply().unwrap();
            (p.root_x, p.root_y)
        };
        let start = at(&conn);
        sleep(Duration::from_millis(300));
        if at(&conn) != start {
            println!("pointer moved by itself before the test: hands off the mouse");
        }
        println!("pointer starts at {start:?}");
        // The pointer is reported in desktop coordinates, the portal takes
        // stream coordinates. The first move finds the stream's offset in the
        // desktop; the other two must land at that offset plus the target.
        let targets = [(w / 2, h / 2), (w / 4, h / 3), (w / 5 * 4, h / 6)];
        let mut offset = (0i32, 0i32);
        let mut ok = 0;
        for (i, (x, y)) in targets.into_iter().enumerate() {
            input.pointer(x, y, 0);
            sleep(Duration::from_millis(500));
            let got = at(&conn);
            let (gx, gy) = (i32::from(got.0), i32::from(got.1));
            if i == 0 {
                offset = (gx - i32::from(x), gy - i32::from(y));
                println!("moved to ({x}, {y}): pointer is at {got:?}, stream offset {offset:?}");
                continue;
            }
            let hit = (gx - offset.0 - i32::from(x)).abs() <= 2
                && (gy - offset.1 - i32::from(y)).abs() <= 2;
            println!(
                "moved to ({x}, {y}): pointer is at {got:?} {}",
                if hit { "ok" } else { "MISMATCH" }
            );
            ok += usize::from(hit);
        }
        // A Shift tap: harmless wherever the keyboard focus is.
        input.key(0xFFE1, true);
        input.key(0xFFE1, false);
        // Back to where it was, if that was on the shared stream.
        let back = (i32::from(start.0) - offset.0, i32::from(start.1) - offset.1);
        match (u16::try_from(back.0), u16::try_from(back.1)) {
            (Ok(x), Ok(y)) if x < w && y < h => {
                input.pointer(x, y, 0);
                sleep(Duration::from_millis(600));
                println!("pointer restored to {:?} (wanted {start:?})", at(&conn));
            }
            _ => println!("start {start:?} is off the shared stream: not restored"),
        }
        println!("portal errors: {:?}", input.first_error());
        println!("{ok} of 2 checked moves arrived");
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    live::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("the Wayland backend is only available on Linux");
}
