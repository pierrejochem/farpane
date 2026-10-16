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
        println!("pointer starts at {start:?}");
        let mut ok = 0;
        for (x, y) in [(w / 2, h / 2), (w / 4, h / 3), (w / 5 * 4, h / 6)] {
            input.pointer(x, y, 0);
            sleep(Duration::from_millis(500));
            let got = at(&conn);
            let hit = i32::from(got.0).abs_diff(i32::from(x)) <= 2
                && i32::from(got.1).abs_diff(i32::from(y)) <= 2;
            println!(
                "moved to ({x}, {y}): pointer is at {got:?} {}",
                if hit { "ok" } else { "MISMATCH" }
            );
            ok += usize::from(hit);
        }
        // A Shift tap: harmless wherever the keyboard focus is.
        input.key(0xFFE1, true);
        input.key(0xFFE1, false);
        // Back to where it was.
        input.pointer(
            u16::try_from(start.0).unwrap_or(0),
            u16::try_from(start.1).unwrap_or(0),
            0,
        );
        sleep(Duration::from_millis(600));
        println!("pointer restored to {:?}", at(&conn));
        println!("portal errors: {:?}", input.first_error());
        println!("{ok} of 3 moves arrived");
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    live::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("the Wayland backend is only available on Linux");
}
