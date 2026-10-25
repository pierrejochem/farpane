//! Interop with macOS Screen Sharing, a real Apple VNC server.
//!
//! Skips unless `FARPANE_MAC=<ssh target>` (for `pbcopy` / `pbpaste`, reached
//! over key-based SSH), `FARPANE_MAC_ADDR=<host:port>` and
//! `FARPANE_MAC_PASSWORD_FILE=<file>` are set. With `FARPANE_MAC_USER=<account>`
//! the viewer signs in with the Mac account (Apple Remote Desktop, type 30),
//! which reaches the logged-in desktop; without it the file holds the "VNC
//! viewers may control screen with password" password (type 2), which only
//! shows the login window. The Mac must be unlocked. The clipboard tests
//! replace the Mac's text clipboard for a moment and put it back; nothing is
//! clicked or typed.

use std::{
    io::Write,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use farpane_viewer::{ConnectOptions, RemoteScreen, ScreenEvent, parse_encodings};

struct Mac {
    ssh: String,
    addr: String,
    user: Option<Vec<u8>>,
    password: Vec<u8>,
}

impl Mac {
    fn from_env() -> Option<Self> {
        let ssh = std::env::var("FARPANE_MAC").ok()?;
        let addr = std::env::var("FARPANE_MAC_ADDR").ok()?;
        let file = std::env::var("FARPANE_MAC_PASSWORD_FILE").ok()?;
        let password = std::fs::read(file).ok()?;
        Some(Self {
            ssh,
            addr,
            user: std::env::var("FARPANE_MAC_USER")
                .ok()
                .map(String::into_bytes),
            password: password.trim_ascii().to_vec(),
        })
    }

    async fn connect(&self, encodings: &str) -> RemoteScreen {
        let wanted = parse_encodings(encodings).unwrap();
        let options = ConnectOptions {
            username: self.user.clone(),
            password: Some(self.password.clone()),
            ..ConnectOptions::default()
        };
        RemoteScreen::connect_opts(&self.addr, &options, &wanted)
            .await
            .unwrap()
    }

    fn pbpaste(&self) -> String {
        let out = Command::new("ssh")
            .args(["-o", "BatchMode=yes", &self.ssh, "pbpaste"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn pbcopy(&self, text: &str) {
        let mut child = Command::new("ssh")
            .args(["-o", "BatchMode=yes", &self.ssh, "LANG=en_US.UTF-8 pbcopy"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }
}

/// Keep the connection alive and collect what the server sends.
async fn pump(screen: &mut RemoteScreen, how_long: Duration) -> Vec<ScreenEvent> {
    let until = tokio::time::Instant::now() + how_long;
    let mut seen = Vec::new();
    while tokio::time::Instant::now() < until {
        screen.request_update(true).await.unwrap();
        if let Ok(Ok(event)) =
            tokio::time::timeout(Duration::from_millis(400), screen.next_event()).await
        {
            seen.push(event);
        }
    }
    seen
}

/// Everything the server sends up to the end of the first update.
async fn first_update(screen: &mut RemoteScreen) -> Vec<ScreenEvent> {
    let mut seen = Vec::new();
    loop {
        let event = screen.next_event().await.unwrap();
        let end = event == ScreenEvent::UpdateEnd;
        seen.push(event);
        if end {
            return seen;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn macos_apple_dh_signs_in() {
    let Some(mac) = Mac::from_env() else {
        return;
    };
    if mac.user.is_none() {
        return;
    }
    let mut screen = mac.connect("zrle,raw,desktopsize,extdesktopsize").await;
    assert_eq!(screen.security(), "Apple Remote Desktop");
    let first = first_update(&mut screen).await;
    assert!(first.iter().any(|e| matches!(e, ScreenEvent::Damage(_))));
}

// Observed on macOS 26.7: the server never sends a Cursor rectangle (the
// pointer is drawn into the picture), so there is no shape to receive.
#[tokio::test(flavor = "current_thread")]
#[ignore = "macOS Screen Sharing sends no cursor shape"]
async fn macos_connects_and_sends_a_cursor_shape() {
    let Some(mac) = Mac::from_env() else {
        return;
    };
    let mut screen = mac
        .connect("zrle,raw,cursor,desktopsize,extdesktopsize")
        .await;
    let first = first_update(&mut screen).await;
    println!(
        "{} / {} / {} {}x{}",
        screen.name(),
        screen.protocol(),
        screen.security(),
        screen.framebuffer().width(),
        screen.framebuffer().height()
    );
    // The server tells about the shape when the pointer moves over a window.
    let mut shape = first.into_iter().find_map(|e| match e {
        ScreenEvent::Cursor(c) => Some(c),
        _ => None,
    });
    for step in 0..20u16 {
        if shape.is_some() {
            break;
        }
        screen
            .pointer(0, 200 + step * 25 % 500, 200 + step * 17 % 300)
            .await
            .unwrap();
        for event in pump(&mut screen, Duration::from_millis(250)).await {
            if let ScreenEvent::Cursor(c) = event {
                shape.get_or_insert(c);
            }
        }
    }
    let shape = shape.expect("the Mac sent no cursor shape");
    println!(
        "cursor shape {}x{} hotspot {:?}",
        shape.width, shape.height, shape.hotspot
    );
}

// Observed on macOS 26.7: text sent with ClientCutText replaces the Mac's
// clipboard with something `pbpaste` reads as empty, and a `pbcopy` on the Mac
// produces no ServerCutText. Not understood yet.
#[tokio::test(flavor = "current_thread")]
#[ignore = "clipboard sync with macOS Screen Sharing does not work yet"]
async fn macos_clipboard_travels_in_both_directions() {
    let Some(mac) = Mac::from_env() else {
        return;
    };
    let original = mac.pbpaste();
    let mut screen = mac
        .connect("zrle,raw,extclipboard,cursor,desktopsize")
        .await;
    let first = first_update(&mut screen).await;
    println!("first update events: {}", first.len());

    // Viewer to Mac.
    let to_mac = "Farpane to the Mac ascii 123";
    let how = screen.send_clipboard(to_mac).await.unwrap();
    println!("sent as {how:?}");
    let until = Instant::now() + Duration::from_secs(10);
    let mut got = mac.pbpaste();
    while got != to_mac && Instant::now() < until {
        pump(&mut screen, Duration::from_millis(500)).await;
        got = mac.pbpaste();
    }
    let forward_ok = got == to_mac;
    println!(
        "viewer to Mac: {} (pbpaste {got:?})",
        if forward_ok { "ok" } else { "FAILED" }
    );

    // Mac to viewer.
    let from_mac = "From the Mac: zurück € ☺";
    mac.pbcopy(from_mac);
    let until = Instant::now() + Duration::from_secs(10);
    let mut received = None;
    while received.is_none() && Instant::now() < until {
        for event in pump(&mut screen, Duration::from_millis(500)).await {
            match event {
                ScreenEvent::Text(t) => received = Some(t.replace("\r\n", "\n")),
                ScreenEvent::CutText(bytes) => {
                    received = Some(bytes.iter().map(|b| char::from(*b)).collect());
                }
                _ => {}
            }
        }
    }
    println!("Mac to viewer: {received:?}");

    mac.pbcopy(&original);
    assert!(forward_ok, "viewer to Mac clipboard");
    assert_eq!(
        received.as_deref(),
        Some(from_mac),
        "Mac to viewer clipboard"
    );
}

/// How fast pictures arrive while the pointer moves about (which makes the
/// Mac redraw). Prints updates per second, bytes, and decode time. Run it
/// with `--release` to see what the viewer does for real.
#[tokio::test(flavor = "current_thread")]
#[ignore = "a measurement, not a check"]
#[allow(clippy::cast_precision_loss)]
async fn macos_update_rate() {
    let Some(mac) = Mac::from_env() else {
        return;
    };
    for encodings in ["zrle,raw", "zlib,raw", "raw"] {
        let mut screen = mac.connect(encodings).await;
        first_update(&mut screen).await;
        let start = Instant::now();
        let (mut updates, mut pixels) = (0u32, 0u64);
        let mut step = 0u16;
        while start.elapsed() < Duration::from_secs(8) {
            step = step.wrapping_add(1);
            screen
                .pointer(0, 600 + step % 400, 500 + step * 3 % 300)
                .await
                .unwrap();
            screen.request_update(true).await.unwrap();
            let Ok(Ok(damage)) =
                tokio::time::timeout(Duration::from_millis(500), screen.next_update()).await
            else {
                continue;
            };
            updates += 1;
            for d in &damage {
                if let rfb_codec::Damage::Rect(r) = d {
                    pixels += u64::from(r.width) * u64::from(r.height);
                }
            }
        }
        let secs = start.elapsed().as_secs_f64();
        let stats = screen.sample_stats(Instant::now());
        println!(
            "{encodings}: {:.1} updates/s, {:.1} Mpixel/s changed, in {:.1} Mbit/s",
            f64::from(updates) / secs,
            pixels as f64 / secs / 1e6,
            stats.rx_per_sec * 8.0 / 1e6,
        );
    }
}
