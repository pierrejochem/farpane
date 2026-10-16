//! TLS certificate trust through the window: ask on first sight, remember the
//! choice, warn loudly when a known server's certificate changes.

use std::{fs, sync::Arc, time::Duration};

use egui::{Context, Pos2, RawInput, Rect as UiRect, Vec2};
use farpane_viewer::app::{Startup, ViewerApp};
use rcgen::{CertificateParams, KeyPair};
use rfb_security::tls::{CertProblem, fingerprint, format_fingerprint};
use rfb_tokio::{PlainVerifier, ServerAuth, ServerOptions, ServerSession};
use rustls::{
    ServerConfig,
    crypto::ring,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::net::{TcpListener, TcpStream};

struct Identity {
    der: Vec<u8>,
    config: Arc<ServerConfig>,
}

fn identity() -> Identity {
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let der = cert.der().to_vec();
    let config = Arc::new(
        ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap(),
    );
    Identity { der, config }
}

fn frame(ctx: &Context, app: &mut ViewerApp) {
    let input = RawInput {
        screen_rect: Some(UiRect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 720.0))),
        focused: true,
        ..RawInput::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

async fn until(ctx: &Context, app: &mut ViewerApp, what: &str, cond: impl Fn(&ViewerApp) -> bool) {
    for _ in 0..600 {
        frame(ctx, app);
        if cond(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A TLS VNC server (no authentication) presenting `id`, accepting every
/// connection. Returns its address.
async fn serve(id: &Identity, auth: ServerAuth) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let config = id.config.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let config = config.clone();
            let auth = auth.clone();
            tokio::spawn(async move {
                let opts = ServerOptions::new(auth, 64, 48, b"Secure PC").with_tls(config);
                if let Ok(mut session) = ServerSession::<TcpStream>::accept(sock, opts).await {
                    while session.next_message().await.is_ok() {}
                }
            });
        }
    });
    addr
}

fn startup(addr: Option<String>, settings: &std::path::Path) -> Startup {
    Startup {
        addr,
        settings_path: Some(settings.to_path_buf()),
        ..Startup::default()
    }
}

#[tokio::test]
async fn a_new_server_asks_and_trusting_it_connects_and_remembers_the_choice() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("viewer.toml");
    let id = identity();
    let addr = serve(&id, ServerAuth::None).await;
    let ctx = Context::default();

    let mut app = ViewerApp::with_context(&ctx, startup(Some(addr.clone()), &settings));
    until(&ctx, &mut app, "the trust question", |a| {
        a.pending_trust().is_some()
    })
    .await;
    assert!(!app.is_connected());
    let (shown, problem) = app.pending_trust().unwrap();
    assert_eq!(
        shown,
        format_fingerprint(&fingerprint(&id.der)),
        "the user sees the real fingerprint"
    );
    assert_eq!(problem, CertProblem::Unknown);
    assert!(
        !settings.exists(),
        "nothing is remembered before the user agrees"
    );

    app.trust_pending();
    until(&ctx, &mut app, "the connection", ViewerApp::is_connected).await;
    assert!(app.pending_trust().is_none());
    let text = fs::read_to_string(&settings).unwrap();
    assert!(
        text.contains("[[known_host]]")
            && text.contains(&format_fingerprint(&fingerprint(&id.der))),
        "{text}"
    );
    assert_eq!(app.saved_connections()[0].0, "Secure PC");

    // Next time, no question.
    drop(app);
    let mut again = ViewerApp::with_context(&ctx, startup(Some(addr), &settings));
    until(
        &ctx,
        &mut again,
        "a silent reconnect",
        ViewerApp::is_connected,
    )
    .await;
    assert!(again.pending_trust().is_none());
}

#[tokio::test]
async fn declining_remembers_nothing_and_stays_on_the_home_screen() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("viewer.toml");
    let id = identity();
    let addr = serve(&id, ServerAuth::None).await;
    let ctx = Context::default();
    let mut app = ViewerApp::with_context(&ctx, startup(Some(addr), &settings));
    until(&ctx, &mut app, "the trust question", |a| {
        a.pending_trust().is_some()
    })
    .await;
    app.reject_pending();
    for _ in 0..30 {
        frame(&ctx, &mut app);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(app.pending_trust().is_none());
    assert!(!app.is_connected());
    assert!(!settings.exists());
}

#[tokio::test]
async fn a_swapped_certificate_is_flagged_as_changed_and_the_old_pin_survives_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("viewer.toml");
    let original = identity();
    let addr = serve(&original, ServerAuth::None).await;
    let ctx = Context::default();
    // Trust the original.
    let mut app = ViewerApp::with_context(&ctx, startup(Some(addr.clone()), &settings));
    until(&ctx, &mut app, "the first question", |a| {
        a.pending_trust().is_some()
    })
    .await;
    app.trust_pending();
    until(&ctx, &mut app, "connection", ViewerApp::is_connected).await;
    drop(app);

    // The same address now presents another certificate: serve it on the same port.
    let imposter = identity();
    let port: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();
    // (The first server task keeps running in this test process, so start the
    // new one on its own port and move the saved entry there.)
    let new_addr = serve(&imposter, ServerAuth::None).await;
    let text = fs::read_to_string(&settings).unwrap().replace(
        &format!(":{port}"),
        &new_addr[new_addr.rfind(':').unwrap()..],
    );
    fs::write(&settings, text).unwrap();

    let mut app = ViewerApp::with_context(&ctx, startup(Some(new_addr.clone()), &settings));
    until(&ctx, &mut app, "the warning", |a| {
        a.pending_trust().is_some()
    })
    .await;
    let (shown, problem) = app.pending_trust().unwrap();
    assert_eq!(problem, CertProblem::Changed);
    assert_eq!(shown, format_fingerprint(&fingerprint(&imposter.der)));

    // Refusing keeps the old pin.
    app.reject_pending();
    let kept = fs::read_to_string(&settings).unwrap();
    assert!(
        kept.contains(&format_fingerprint(&fingerprint(&original.der))),
        "{kept}"
    );
    assert!(!kept.contains(&format_fingerprint(&fingerprint(&imposter.der))));

    // Accepting replaces it.
    let mut app = ViewerApp::with_context(&ctx, startup(Some(new_addr), &settings));
    until(&ctx, &mut app, "the warning again", |a| {
        a.pending_trust().is_some()
    })
    .await;
    app.trust_pending();
    until(&ctx, &mut app, "connection", ViewerApp::is_connected).await;
    let after = fs::read_to_string(&settings).unwrap();
    assert!(
        after.contains(&format_fingerprint(&fingerprint(&imposter.der))),
        "{after}"
    );
    assert!(
        !after.contains(&format_fingerprint(&fingerprint(&original.der))),
        "the old pin is retired: {after}"
    );
}

#[tokio::test]
async fn username_and_password_reach_an_x509_plain_server() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("viewer.toml");
    let id = identity();
    let verifier = PlainVerifier::new(|u, p| u == b"alice" && p == b"s3cret");
    let addr = serve(&id, ServerAuth::Plain(verifier)).await;
    let ctx = Context::default();

    let mut app = ViewerApp::with_context(&ctx, startup(None, &settings));
    app.set_credentials("alice", "s3cret");
    app.set_address(&addr);
    app.start_connect();
    until(&ctx, &mut app, "the trust question", |a| {
        a.pending_trust().is_some()
    })
    .await;
    app.trust_pending();
    until(&ctx, &mut app, "the login", ViewerApp::is_connected).await;
}
