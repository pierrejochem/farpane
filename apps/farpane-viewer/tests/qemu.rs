//! Interop with a real server: QEMU's built-in VNC server.
//!
//! Needs `qemu-system-x86_64` on PATH; the tests skip (and say so) without it.
//! QEMU boots no disk, so the screen is the BIOS text mode, 720 x 400.

use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use farpane_viewer::{ConnectError, RemoteScreen, parse_encodings};
use rfb_codec::Framebuffer;
use rfb_tokio::Error;

struct Qemu(Child);

impl Drop for Qemu {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start QEMU with a VNC server on a free port; `None` if QEMU is missing.
fn start(password: Option<&str>) -> Option<(Qemu, String)> {
    let present = Command::new("qemu-system-x86_64")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !present {
        eprintln!("qemu-system-x86_64 not found: skipping interop test");
        return None;
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()?
        .port();
    let display = port - 5900;
    let mut cmd = Command::new("qemu-system-x86_64");
    // No network card: iPXE would otherwise sit in DHCP for seconds and keep
    // changing the screen after it looks settled.
    cmd.args(["-display", "none", "-m", "64", "-no-reboot", "-nic", "none"]);
    match password {
        Some(pw) => {
            cmd.args(["-object", &format!("secret,id=sec0,data={pw}")]);
            cmd.args(["-vnc", &format!("127.0.0.1:{display},password-secret=sec0")]);
        }
        None => {
            cmd.args(["-vnc", &format!("127.0.0.1:{display}")]);
        }
    }
    let child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let qemu = Qemu(child);
    let addr = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(Instant::now() < deadline, "QEMU did not open {addr}");
        std::thread::sleep(Duration::from_millis(100));
    }
    Some((qemu, addr))
}

/// Apply updates for `span`, so the picture follows the server.
async fn follow(screen: &mut RemoteScreen, span: Duration) {
    let deadline = tokio::time::Instant::now() + span;
    loop {
        screen.request_update(true).await.unwrap();
        if tokio::time::timeout_at(deadline, screen.next_update())
            .await
            .is_err()
        {
            return;
        }
    }
}

/// QEMU starts in 640 x 480 graphics mode, switches to the 720 x 400 text
/// screen while booting (announced by a `DesktopSize` rectangle) and keeps
/// printing boot messages for a few seconds. Watch one connection through the
/// resize (which also tests resizing against a real server), then wait until
/// the text stops changing, so later captures all see the same screen.
async fn wait_for_bios(addr: &str) {
    let mut screen =
        RemoteScreen::connect_with(addr, None, &parse_encodings("raw,desktopsize").unwrap())
            .await
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut previous: Option<Framebuffer> = None;
    while Instant::now() < deadline {
        follow(&mut screen, Duration::from_millis(1500)).await;
        let fb = screen.framebuffer().clone();
        let size_ok = (fb.width(), fb.height()) == (720, 400);
        if size_ok && lit_pixels(&fb) > 1_000 {
            // Only the blinking cursor may change between two looks.
            if previous
                .as_ref()
                .is_some_and(|p| differing_pixels(p, &fb) <= 64)
            {
                return;
            }
        }
        previous = Some(fb);
    }
    panic!("the BIOS screen never settled");
}

/// Connect, take the first full update, then updates for a short while.
async fn capture(addr: &str, password: Option<&str>, encodings: &str) -> Framebuffer {
    let mut screen = RemoteScreen::connect_with(
        addr,
        password.map(|p| p.as_bytes().to_vec()),
        &parse_encodings(encodings).unwrap(),
    )
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
    screen.framebuffer().clone()
}

fn lit_pixels(fb: &Framebuffer) -> usize {
    fb.pixels()
        .chunks_exact(4)
        .filter(|p| p[..3] != [0, 0, 0])
        .count()
}

fn differing_pixels(a: &Framebuffer, b: &Framebuffer) -> usize {
    a.pixels()
        .chunks_exact(4)
        .zip(b.pixels().chunks_exact(4))
        .filter(|(x, y)| x != y)
        .count()
}

#[tokio::test(flavor = "current_thread")]
async fn every_encoding_shows_the_same_bios_screen() {
    let Some((_qemu, addr)) = start(None) else {
        return;
    };
    wait_for_bios(&addr).await;
    let raw = capture(&addr, None, "raw").await;
    assert_eq!((raw.width(), raw.height()), (720, 400));
    // The BIOS prints text, so a real picture has many lit pixels.
    assert!(
        lit_pixels(&raw) > 1_000,
        "screen looks empty: {}",
        lit_pixels(&raw)
    );
    for encodings in [
        "hextile,raw",
        "zlib,raw",
        "zrle,raw",
        "zrle,hextile,zlib,corre,copyrect,raw",
    ] {
        let fb = capture(&addr, None, encodings).await;
        // Only the blinking text cursor (a few pixels) may differ.
        let diff = differing_pixels(&raw, &fb);
        if diff > 64 {
            if let Some(dir) = std::env::var_os("FARPANE_DUMP_DIR") {
                let dir = std::path::PathBuf::from(dir);
                farpane_viewer::write_image(&raw, &dir.join("raw.ppm")).unwrap();
                farpane_viewer::write_image(&fb, &dir.join("other.ppm")).unwrap();
            }
        }
        assert!(diff <= 64, "{encodings}: {diff} pixels differ from Raw");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn vnc_password_authentication_works_against_qemu() {
    let Some((_qemu, addr)) = start(Some("hunter2")) else {
        return;
    };
    let fb = capture(&addr, Some("hunter2"), "zrle,raw").await;
    assert!(lit_pixels(&fb) > 1_000);

    let wrong = RemoteScreen::connect(&addr, Some(b"nope".to_vec())).await;
    assert!(
        matches!(&wrong, Err(ConnectError::Session(Error::Proto(e))) if e.to_string().contains("security failed")),
        "{wrong:?}"
    );
    let missing = RemoteScreen::connect(&addr, None).await;
    assert!(
        matches!(missing, Err(ConnectError::Session(Error::PasswordRequired))),
        "{missing:?}"
    );
}
