//! Interop with a real server: the `Xvnc` of `TigerVNC`.
//!
//! Needs `Xvnc`, `vncpasswd`, `xmessage`, `xsetroot` and `xclip` on PATH; the
//! tests skip (and say so) without them. `Xvnc` is an X server of its own, so
//! nothing here touches the desktop the tests run on.

use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use farpane_viewer::{ConnectError, RemoteScreen, ScreenEvent, parse_encodings};
use rfb_codec::Framebuffer;
use rfb_tokio::Error;

const PASSWORD: &str = "interop-pass";

struct Xvnc {
    child: Child,
    /// X display number, for the X clients started against it.
    display: String,
    helpers: Vec<Child>,
    dir: PathBuf,
}

impl Drop for Xvnc {
    fn drop(&mut self) {
        for h in &mut self.helpers {
            let _ = h.kill();
            let _ = h.wait();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn have(tool: &str) -> bool {
    Command::new("which")
        .arg(tool)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Xvnc {
    /// Start `Xvnc` with VNC Auth on a free port; `None` if a tool is missing.
    fn start() -> Option<(Self, String)> {
        for tool in ["Xvnc", "vncpasswd", "xmessage", "xsetroot", "xclip"] {
            if !have(tool) {
                eprintln!("{tool} not found: skipping TigerVNC interop test");
                return None;
            }
        }
        let port = free_port();
        // Display numbers are only a name for the X socket; pick one from the port.
        let display = format!(":{}", 200 + port % 700);
        let dir = std::env::temp_dir().join(format!("farpane-tigervnc-{port}"));
        std::fs::create_dir_all(&dir).ok()?;
        let passwd = dir.join("passwd");
        let mut vncpasswd = Command::new("vncpasswd")
            .arg("-f")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        {
            use std::io::Write;
            writeln!(vncpasswd.stdin.take()?, "{PASSWORD}").ok()?;
        }
        std::fs::write(&passwd, vncpasswd.wait_with_output().ok()?.stdout).ok()?;
        let child = Command::new("Xvnc")
            .args([&display, "-rfbport", &port.to_string(), "-localhost"])
            .args(["-SecurityTypes", "VncAuth", "-rfbauth"])
            .arg(&passwd)
            .args(["-geometry", "800x600", "-depth", "24", "-AlwaysShared"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut x = Self {
            child,
            display,
            helpers: Vec::new(),
            dir,
        };
        let addr = format!("127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::net::TcpStream::connect(&addr).is_err() {
            assert!(Instant::now() < deadline, "Xvnc did not open {addr}");
            std::thread::sleep(Duration::from_millis(100));
        }
        // Something to look at: a background colour and a window of text.
        assert!(
            x.run("xsetroot", &["-solid", "#335577"]).success(),
            "xsetroot"
        );
        x.helpers.push(
            x.command("xmessage")
                .args(["-geometry", "+20+20", "-center"])
                .arg("Farpane interop: TigerVNC Xvnc\n0123456789 abcdefghijklmnopqrstuvwxyz")
                .spawn()
                .ok()?,
        );
        std::thread::sleep(Duration::from_millis(800));
        Some((x, addr))
    }

    fn command(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        c.env("DISPLAY", &self.display)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        c
    }

    fn run(&self, program: &str, args: &[&str]) -> std::process::ExitStatus {
        self.command(program).args(args).status().unwrap()
    }

    /// Text on the X clipboard, as `xclip` reads it. Xvnc may have to ask the
    /// viewer for it first, so the viewer is served while `xclip` waits.
    async fn clipboard(&self, screen: &mut RemoteScreen) -> String {
        let mut child = self
            .command("xclip")
            .args(["-selection", "clipboard", "-o"])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(100), screen.next_event()).await;
        }
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            use std::io::Read;
            let _ = stdout.read_to_string(&mut out);
        }
        let _ = child.wait();
        out
    }
}

async fn capture(addr: &str, encodings: &str) -> Framebuffer {
    let wanted = parse_encodings(encodings).unwrap();
    let mut screen = RemoteScreen::connect_with(addr, Some(PASSWORD.as_bytes().to_vec()), &wanted)
        .await
        .unwrap();
    screen.next_update().await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(600);
    loop {
        screen.request_update(true).await.unwrap();
        if tokio::time::timeout_at(deadline, screen.next_update())
            .await
            .is_err()
        {
            break;
        }
    }
    let used = screen.sample_stats(std::time::Instant::now());
    assert!(
        used.encodings
            .iter()
            .any(|e| e.encoding == wanted[0] && e.rects > 0),
        "{encodings}: server did not use {}: {:?}",
        wanted[0],
        used.encodings
    );
    screen.framebuffer().clone()
}

fn distinct_colours(fb: &Framebuffer) -> usize {
    let mut seen = std::collections::HashSet::new();
    for px in fb.pixels().as_chunks::<4>().0 {
        seen.insert([px[0], px[1], px[2]]);
    }
    seen.len()
}

#[tokio::test(flavor = "current_thread")]
async fn every_encoding_shows_the_same_screen() {
    let Some((_x, addr)) = Xvnc::start() else {
        return;
    };
    let raw = capture(&addr, "raw").await;
    assert_eq!((raw.width(), raw.height()), (800, 600));
    // A background colour, the window frame and anti-aliased text.
    assert!(distinct_colours(&raw) > 3, "the screen has nothing on it");
    for encoding in ["zrle", "tight", "hextile"] {
        let other = capture(&addr, encoding).await;
        assert_eq!(
            raw.pixels(),
            other.pixels(),
            "{encoding} differs from raw against TigerVNC"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_wrong_password_is_refused_cleanly() {
    let Some((_x, addr)) = Xvnc::start() else {
        return;
    };
    let wrong = RemoteScreen::connect(&addr, Some(b"nope".to_vec())).await;
    assert!(
        matches!(&wrong, Err(ConnectError::Session(Error::Proto(e))) if e.to_string().contains("security failed")),
        "{wrong:?}"
    );
    assert!(RemoteScreen::connect(&addr, None).await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn a_resize_request_is_granted() {
    let Some((_x, addr)) = Xvnc::start() else {
        return;
    };
    let wanted = parse_encodings("raw,extdesktopsize,desktopsize").unwrap();
    let mut screen = RemoteScreen::connect_with(&addr, Some(PASSWORD.as_bytes().to_vec()), &wanted)
        .await
        .unwrap();
    screen.next_update().await.unwrap();
    assert!(
        screen.request_size(1024, 700).await.unwrap(),
        "request sent"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut granted = false;
    while !granted && tokio::time::Instant::now() < deadline {
        screen.request_update(true).await.unwrap();
        if let Ok(Ok(ScreenEvent::Layout(l))) =
            tokio::time::timeout(Duration::from_millis(500), screen.next_event()).await
        {
            granted = l.reason == 1 && l.status == 0;
        }
    }
    assert!(granted, "TigerVNC did not grant the resize");
    assert_eq!(
        (screen.framebuffer().width(), screen.framebuffer().height()),
        (1024, 700)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn utf8_text_travels_in_both_directions() {
    let Some((x, addr)) = Xvnc::start() else {
        return;
    };
    let wanted = parse_encodings("raw,extclipboard").unwrap();
    let mut screen = RemoteScreen::connect_with(&addr, Some(PASSWORD.as_bytes().to_vec()), &wanted)
        .await
        .unwrap();
    screen.next_update().await.unwrap();
    let text = "grüße € ☺ from the viewer";
    screen.send_clipboard(text).await.unwrap();
    // The text reaches the X clipboard once the server has asked for it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got = String::new();
    while got != text && Instant::now() < deadline {
        screen.request_update(true).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_millis(300), screen.next_event()).await;
        got = x.clipboard(&mut screen).await;
    }
    assert_eq!(got, text, "viewer to Xvnc clipboard");

    // And back: an X client copies, the viewer receives it.
    let mut owners = Vec::new();
    for selection in ["clipboard", "primary"] {
        let mut owner = x
            .command("xclip")
            .args(["-selection", selection, "-i"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            owner
                .stdin
                .take()
                .unwrap()
                .write_all("zurück ☺ from X".as_bytes())
                .unwrap();
        }
        owners.push(owner);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut received = None;
    while received.is_none() && tokio::time::Instant::now() < deadline {
        screen.request_update(true).await.unwrap();
        match tokio::time::timeout(Duration::from_millis(500), screen.next_event()).await {
            Ok(Ok(ScreenEvent::Text(t))) => received = Some(t),
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(e)) => panic!("viewer error while waiting for clipboard: {e}"),
        }
    }
    for mut owner in owners {
        let _ = owner.kill();
        let _ = owner.wait();
    }
    assert_eq!(
        received.as_deref().map(|t| t.replace("\r\n", "\n")),
        Some("zurück ☺ from X".to_owned()),
        "Xvnc clipboard to viewer"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_pointer_shape_is_decoded() {
    let Some((_x, addr)) = Xvnc::start() else {
        return;
    };
    let wanted = parse_encodings("raw,cursor").unwrap();
    let mut screen = RemoteScreen::connect_with(&addr, Some(PASSWORD.as_bytes().to_vec()), &wanted)
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut shape = None;
    let mut step = 0u16;
    while shape.is_none() && tokio::time::Instant::now() < deadline {
        // The server tells about the pointer's shape when the pointer moves
        // (over the root window and over the message window).
        step += 1;
        screen
            .pointer(0, 100 + step * 20 % 600, 80 + step * 15 % 400)
            .await
            .unwrap();
        screen.request_update(false).await.unwrap();
        if let Ok(Ok(ScreenEvent::Cursor(c))) =
            tokio::time::timeout(Duration::from_millis(500), screen.next_event()).await
        {
            // An empty shape means "hidden", which is a valid first message.
            if c.width > 0 && c.height > 0 {
                shape = Some(c);
            }
        }
    }
    let shape = shape.expect("Xvnc sent no visible cursor shape");
    assert!(shape.width > 0 && shape.height > 0);
    assert_eq!(
        shape.rgba.len(),
        usize::from(shape.width) * usize::from(shape.height) * 4
    );
    assert!(
        shape.rgba.as_chunks::<4>().0.iter().any(|p| p[3] == 255),
        "no opaque pixel"
    );
}
