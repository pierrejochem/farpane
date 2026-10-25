//! Live check of the Wayland backend's clipboard, both ways, with `xclip`
//! as the other program (it talks to the clipboard through `XWayland`).
//! It replaces the desktop's text clipboard for a few seconds and puts the
//! old text back at the end.
//!
//! `cargo run -p farpane-wayland --example probe_clipboard` (answer the
//! portal dialog).

#[cfg(target_os = "linux")]
mod live {
    use std::{
        io::Write,
        process::{Command, Stdio},
        thread::sleep,
        time::{Duration, Instant},
    };

    use farpane_wayland::{Config, open};
    use rfb_codec::Framebuffer;
    use rfb_server::{Input, Source};
    use x11rb::{
        COPY_DEPTH_FROM_PARENT,
        connection::Connection,
        protocol::xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass},
        wrapper::ConnectionExt as _,
    };

    fn xclip_read() -> Option<String> {
        let out = Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn xclip_write(text: &str) {
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-i"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("xclip");
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(text.as_bytes()).unwrap();
        drop(stdin);
        // The foreground part ends at once; a copy stays behind to serve the
        // text until something replaces it.
        child.wait().unwrap();
    }

    pub fn main() {
        let token = std::env::temp_dir().join("farpane-probe-token");
        // A window of our own for the approval dialog to belong to.
        let (conn, num) = x11rb::connect(None).expect("X display");
        let root = conn.setup().roots[num].root;
        let win = conn.generate_id().unwrap();
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            400,
            120,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap();
        conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Farpane clipboard probe",
        )
        .unwrap();
        conn.map_window(win).unwrap();
        conn.flush().unwrap();
        let (mut source, input) = open(&Config {
            restore_token_file: Some(token),
            parent_window: Some(win.into()),
        })
        .expect("open");
        let (w, h) = source.size();
        let mut fb = Framebuffer::new(w, h).unwrap();
        println!(
            "sharing {w}x{h}, clipboard portal: {}",
            input.has_clipboard()
        );
        if !input.has_clipboard() {
            return;
        }
        let original = xclip_read();
        // Settle: the first capture may carry text that was already there.
        sleep(Duration::from_millis(500));
        let _ = source.capture(&mut fb);

        // Client to desktop, and no echo back to the clients.
        let to_desktop = "Farpane to the desktop: ä ö ü € 😀";
        input.clipboard(to_desktop);
        let until = Instant::now() + Duration::from_millis(1500);
        let mut echoed = None;
        while Instant::now() < until {
            if let Some(t) = source.capture(&mut fb).unwrap().clipboard {
                echoed = Some(t);
            }
            sleep(Duration::from_millis(50));
        }
        let read = xclip_read();
        println!(
            "client to desktop: {} (read {read:?})",
            if read.as_deref() == Some(to_desktop) {
                "ok"
            } else {
                "MISSING"
            }
        );
        println!(
            "no echo: {}",
            if echoed.is_none() {
                "ok".to_owned()
            } else {
                format!("ECHOED {echoed:?}")
            }
        );

        // Desktop to client.
        let from_desktop = "From the desktop: ß ñ ø € 🎉";
        xclip_write(from_desktop);
        let until = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while Instant::now() < until && got.is_none() {
            got = source.capture(&mut fb).unwrap().clipboard;
            sleep(Duration::from_millis(50));
        }
        println!(
            "desktop to client: {} (got {got:?})",
            if got.as_deref() == Some(from_desktop) {
                "ok"
            } else {
                "MISSING"
            }
        );
        println!("portal errors: {:?}", input.first_error());

        match original {
            Some(text) if !text.is_empty() => {
                xclip_write(&text);
                println!("clipboard text put back ({} bytes)", text.len());
            }
            _ => println!("the clipboard had no text before; test text is still on it"),
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    live::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("the Wayland backend is only available on Linux");
}
