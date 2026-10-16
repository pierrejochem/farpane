//! Interop with a real server: QEMU's built-in VNC server.
//!
//! Needs `qemu-system-x86_64` on PATH; the tests skip (and say so) without it.
//! QEMU boots no disk, so the screen is the BIOS text mode, 720 x 400.

use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use farpane_viewer::{ConnectError, ConnectOptions, RemoteScreen, parse_encodings};
use rfb_codec::Framebuffer;
use rfb_security::tls::{CertProblem, fingerprint};
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

// --- VeNCrypt TLS against QEMU's own (gnutls) implementation ----------------

/// A throwaway CA and a server certificate for 127.0.0.1 / localhost, written
/// where QEMU's `tls-creds-x509` looks for them.
struct Certs {
    dir: tempfile::TempDir,
    ca_der: Vec<u8>,
    leaf_der: Vec<u8>,
}

fn make_certs() -> Certs {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose, SanType,
    };
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Farpane test CA");
    let ca = ca_params.clone().self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let leaf_key = KeyPair::generate().unwrap();
    let mut leaf_params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    leaf_params
        .subject_alt_names
        .push(SanType::IpAddress("127.0.0.1".parse().unwrap()));
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ca-cert.pem"), ca.pem()).unwrap();
    std::fs::write(dir.path().join("server-cert.pem"), leaf.pem()).unwrap();
    std::fs::write(dir.path().join("server-key.pem"), leaf_key.serialize_pem()).unwrap();
    Certs {
        dir,
        ca_der: ca.der().to_vec(),
        leaf_der: leaf.der().to_vec(),
    }
}

/// Start QEMU with a TLS-protected VNC server (certificate validated, no
/// client certificate), optionally with a password. `None` without QEMU.
fn start_tls(certs: &Certs, password: Option<&str>) -> Option<(Qemu, String)> {
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
    cmd.args(["-display", "none", "-m", "64", "-no-reboot", "-nic", "none"]);
    cmd.args([
        "-object",
        &format!(
            "tls-creds-x509,id=tls0,endpoint=server,dir={},verify-peer=no",
            certs.dir.path().display()
        ),
    ]);
    let mut vnc = format!("127.0.0.1:{display},tls-creds=tls0");
    if let Some(pw) = password {
        cmd.args(["-object", &format!("secret,id=sec0,data={pw}")]);
        vnc.push_str(",password-secret=sec0");
    }
    cmd.args(["-vnc", &vnc]);
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

/// Connect with `options` and follow the server until the BIOS has drawn
/// something, returning how many pixels are lit.
async fn lit_after_connecting(addr: &str, options: &ConnectOptions) -> usize {
    let mut screen =
        RemoteScreen::connect_opts(addr, options, &parse_encodings("zrle,raw").unwrap())
            .await
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        follow(&mut screen, Duration::from_millis(400)).await;
        let lit = lit_pixels(screen.framebuffer());
        if lit > 1_000 || Instant::now() > deadline {
            return lit;
        }
    }
}

fn untrusted(result: &Result<RemoteScreen, ConnectError>) -> Option<([u8; 32], CertProblem)> {
    match result {
        Err(ConnectError::Session(rfb_tokio::Error::Untrusted {
            fingerprint,
            problem,
            ..
        })) => Some((*fingerprint, problem.clone())),
        _ => None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn qemu_tls_asks_first_then_works_once_the_certificate_is_pinned() {
    let certs = make_certs();
    let Some((_qemu, addr)) = start_tls(&certs, None) else {
        return;
    };

    // The CA is not known to the viewer and nothing is pinned: refused, with
    // the fingerprint of exactly the certificate QEMU presented.
    let first = RemoteScreen::connect_opts(&addr, &ConnectOptions::default(), &[0]).await;
    let (fp, problem) = untrusted(&first).expect("an untrusted-certificate error");
    assert_eq!(fp, fingerprint(&certs.leaf_der));
    assert_eq!(problem, CertProblem::Unknown);

    // Pinned: connected, encrypted end to end, picture arrives.
    let options = ConnectOptions {
        pins: vec![fp],
        ..ConnectOptions::default()
    };
    assert!(lit_after_connecting(&addr, &options).await > 1_000);

    // A different pin means a different certificate than the one trusted.
    let wrong = ConnectOptions {
        pins: vec![[0xAB; 32]],
        ..ConnectOptions::default()
    };
    let second = RemoteScreen::connect_opts(&addr, &wrong, &[0]).await;
    assert_eq!(
        untrusted(&second).map(|(_, p)| p),
        Some(CertProblem::Changed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn qemu_tls_certificate_signed_by_a_trusted_private_ca_needs_no_pin() {
    let certs = make_certs();
    let Some((_qemu, addr)) = start_tls(&certs, None) else {
        return;
    };
    let options = ConnectOptions {
        extra_roots: vec![certs.ca_der.clone()],
        ..ConnectOptions::default()
    };
    assert!(lit_after_connecting(&addr, &options).await > 1_000);
}

#[tokio::test(flavor = "current_thread")]
async fn qemu_tls_with_a_password_uses_x509_vnc() {
    let certs = make_certs();
    let Some((_qemu, addr)) = start_tls(&certs, Some("hunter2")) else {
        return;
    };
    let pins = vec![fingerprint(&certs.leaf_der)];
    let ok = ConnectOptions {
        password: Some(b"hunter2".to_vec()),
        pins: pins.clone(),
        ..ConnectOptions::default()
    };
    assert!(lit_after_connecting(&addr, &ok).await > 1_000);

    let wrong = ConnectOptions {
        password: Some(b"nope".to_vec()),
        pins: pins.clone(),
        ..ConnectOptions::default()
    };
    let r = RemoteScreen::connect_opts(&addr, &wrong, &[0]).await;
    assert!(
        matches!(&r, Err(ConnectError::Session(rfb_tokio::Error::Proto(_)))),
        "{:?}",
        r.err()
    );

    let missing = ConnectOptions {
        pins,
        ..ConnectOptions::default()
    };
    let r = RemoteScreen::connect_opts(&addr, &missing, &[0]).await;
    assert!(
        matches!(
            &r,
            Err(ConnectError::Session(rfb_tokio::Error::PasswordRequired))
        ),
        "{:?}",
        r.err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn qemu_announces_its_screen_layout_when_asked() {
    use farpane_viewer::ScreenEvent;
    let Some((_qemu, addr)) = start(None) else {
        return;
    };
    let mut screen = RemoteScreen::connect(&addr, None).await.unwrap();
    // The server must answer our first (non-incremental) request with an
    // ExtendedDesktopSize rectangle (spec), which is how the client learns that
    // SetDesktopSize is available at all.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut layout = None;
    while layout.is_none() && tokio::time::Instant::now() < deadline {
        screen.request_update(true).await.unwrap();
        if let Ok(Ok(ScreenEvent::Layout(l))) =
            tokio::time::timeout(Duration::from_millis(500), screen.next_event()).await
        {
            layout = Some(l);
        }
    }
    let l = layout.expect("QEMU sent no ExtendedDesktopSize rectangle");
    assert_eq!(l.status, 0);
    assert!(!l.screens.is_empty(), "at least one screen is described");
    assert_eq!(
        (l.width, l.height),
        (screen.framebuffer().width(), screen.framebuffer().height())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn qemu_answers_a_resize_request_even_though_a_bios_guest_cannot_comply() {
    use farpane_viewer::ScreenEvent;
    let Some((_qemu, addr)) = start(None) else {
        return;
    };
    let mut screen = RemoteScreen::connect(&addr, None).await.unwrap();
    // Wait for the layout announcement first: SetDesktopSize is not allowed before.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut announced = false;
    while !announced && tokio::time::Instant::now() < deadline {
        screen.request_update(true).await.unwrap();
        announced = matches!(
            tokio::time::timeout(Duration::from_millis(500), screen.next_event()).await,
            Ok(Ok(ScreenEvent::Layout(_)))
        );
    }
    assert!(announced);
    assert!(
        screen.request_size(1024, 768).await.unwrap(),
        "the request was sent"
    );
    // The spec says every SetDesktopSize is answered with a rectangle for the
    // requester (reason 1), granted or not.
    let mut answer = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while answer.is_none() && tokio::time::Instant::now() < deadline {
        if let Ok(Ok(ScreenEvent::Layout(l))) =
            tokio::time::timeout(Duration::from_millis(500), screen.next_event()).await
        {
            if l.reason == 1 {
                answer = Some(l);
            }
        }
    }
    let l = answer.expect("no answer to SetDesktopSize");
    eprintln!(
        "QEMU answered: reason {} status {} size {}x{}",
        l.reason, l.status, l.width, l.height
    );
    assert!(
        matches!(l.status, 0..=4),
        "a defined status code: {}",
        l.status
    );
}
