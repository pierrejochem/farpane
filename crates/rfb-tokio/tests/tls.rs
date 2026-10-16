//! `VeNCrypt` over TLS: every subtype, certificate trust and the retry flow,
//! and proof that the traffic really is encrypted.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rcgen::{CertificateParams, KeyPair};
use rfb_proto::msg::{client::ClientMessage, server::ServerMessage};
use rfb_security::tls::{CertProblem, fingerprint};
use rfb_tokio::{
    ClientOptions, ClientSession, Error, PlainVerifier, ServerAuth, ServerOptions, ServerSession,
    TlsOptions,
};
use rustls::{
    ServerConfig,
    crypto::ring,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, duplex},
    net::{TcpListener, TcpStream},
};

struct Identity {
    der: Vec<u8>,
    config: Arc<ServerConfig>,
}

fn identity(name: &str) -> Identity {
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec![name.to_owned()])
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

fn server_opts(id: &Identity, auth: ServerAuth) -> ServerOptions {
    ServerOptions::new(auth, 64, 48, b"tls test").with_tls(id.config.clone())
}

fn client_opts(pins: Vec<[u8; 32]>) -> ClientOptions {
    ClientOptions {
        tls: Some(TlsOptions {
            server_name: "localhost".into(),
            pins,
            extra_roots: vec![],
        }),
        ..ClientOptions::default()
    }
}

#[tokio::test]
async fn x509_none_and_x509_vnc_and_x509_plain_all_connect_and_carry_traffic() {
    let id = identity("localhost");
    let pin = vec![fingerprint(&id.der)];
    let plain = PlainVerifier::new(|u, p| u == b"alice" && p == b"s3cret");
    let cases: Vec<(ServerAuth, ClientOptions)> = vec![
        (ServerAuth::None, client_opts(pin.clone())),
        (
            ServerAuth::VncPassword(b"hunter2".to_vec()),
            ClientOptions {
                password: Some(b"hunter2".to_vec()),
                ..client_opts(pin.clone())
            },
        ),
        (
            ServerAuth::Plain(plain),
            ClientOptions {
                username: Some(b"alice".to_vec()),
                password: Some(b"s3cret".to_vec()),
                ..client_opts(pin.clone())
            },
        ),
    ];
    for (auth, copts) in cases {
        let (c, s) = duplex(4096);
        let server = tokio::spawn(ServerSession::accept(s, server_opts(&id, auth.clone())));
        let (mut client, info) = ClientSession::connect(c, copts).await.unwrap();
        let mut server = server.await.unwrap().unwrap();
        assert_eq!(info.name, b"tls test", "{auth:?}");
        assert!(client.is_encrypted() && server.is_encrypted(), "{auth:?}");
        // Messages flow both ways inside TLS.
        client
            .send(&ClientMessage::ClientCutText(b"over tls".to_vec()))
            .await
            .unwrap();
        assert_eq!(
            server.next_message().await.unwrap(),
            ClientMessage::ClientCutText(b"over tls".to_vec())
        );
        server.send(&ServerMessage::Bell).await.unwrap();
        client.next_event().await.unwrap();
    }
}

#[tokio::test]
async fn wrong_credentials_are_rejected_over_tls() {
    let id = identity("localhost");
    let pin = vec![fingerprint(&id.der)];
    // Wrong VNC password.
    let (c, s) = duplex(4096);
    let server = tokio::spawn(ServerSession::accept(
        s,
        server_opts(&id, ServerAuth::VncPassword(b"right".to_vec())),
    ));
    let err = ClientSession::connect(
        c,
        ClientOptions {
            password: Some(b"wrong".to_vec()),
            ..client_opts(pin.clone())
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, Error::Proto(e) if e.to_string().contains("authentication failed")),
        "{err}"
    );
    assert!(matches!(
        server.await.unwrap().unwrap_err(),
        Error::AuthFailed
    ));

    // Wrong username or password for Plain.
    let (c, s) = duplex(4096);
    let verifier = PlainVerifier::new(|u, p| u == b"alice" && p == b"s3cret");
    let server = tokio::spawn(ServerSession::accept(
        s,
        server_opts(&id, ServerAuth::Plain(verifier)),
    ));
    let err = ClientSession::connect(
        c,
        ClientOptions {
            username: Some(b"mallory".to_vec()),
            password: Some(b"s3cret".to_vec()),
            ..client_opts(pin.clone())
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(&err, Error::Proto(_)), "{err}");
    assert!(matches!(
        server.await.unwrap().unwrap_err(),
        Error::AuthFailed
    ));

    // No username at all for a server that wants one.
    let (c, s) = duplex(4096);
    let verifier = PlainVerifier::new(|_, _| true);
    tokio::spawn(ServerSession::accept(
        s,
        server_opts(&id, ServerAuth::Plain(verifier)),
    ));
    let err = ClientSession::connect(
        c,
        ClientOptions {
            password: Some(b"x".to_vec()),
            ..client_opts(pin)
        },
    )
    .await
    .unwrap_err();
    // With only a password the client prefers X509Vnc, which a Plain-only
    // server does not offer: it never sends credentials it has no username for.
    assert!(
        matches!(err, Error::Proto(_) | Error::UsernameRequired),
        "{err}"
    );
}

#[tokio::test]
async fn an_untrusted_certificate_comes_back_with_its_fingerprint_and_a_pin_fixes_it() {
    let id = identity("localhost");
    // First visit: nothing pinned.
    let (c, s) = duplex(4096);
    tokio::spawn(ServerSession::accept(s, server_opts(&id, ServerAuth::None)));
    let err = ClientSession::connect(c, client_opts(vec![]))
        .await
        .unwrap_err();
    let Error::Untrusted {
        host,
        fingerprint: fp,
        problem,
    } = err
    else {
        panic!("expected an untrusted certificate");
    };
    assert_eq!(
        (host.as_str(), problem),
        ("localhost", CertProblem::Unknown)
    );
    assert_eq!(fp, fingerprint(&id.der));

    // The user trusts it; the caller pins the fingerprint and connects again.
    let (c, s) = duplex(4096);
    let server = tokio::spawn(ServerSession::accept(s, server_opts(&id, ServerAuth::None)));
    ClientSession::connect(c, client_opts(vec![fp]))
        .await
        .unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_changed_certificate_is_flagged_as_changed() {
    let original = identity("localhost");
    let imposter = identity("localhost");
    let (c, s) = duplex(4096);
    tokio::spawn(ServerSession::accept(
        s,
        server_opts(&imposter, ServerAuth::None),
    ));
    let err = ClientSession::connect(c, client_opts(vec![fingerprint(&original.der)]))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Untrusted { problem: CertProblem::Changed, fingerprint: fp, .. } if *fp == fingerprint(&imposter.der)),
        "{err}"
    );
    assert!(err.to_string().contains("has changed"), "{err}");
}

#[tokio::test]
async fn without_tls_options_a_tls_only_server_is_refused_not_spoken_to_in_the_clear() {
    let id = identity("localhost");
    let (c, s) = duplex(4096);
    tokio::spawn(ServerSession::accept(s, server_opts(&id, ServerAuth::None)));
    let err = ClientSession::connect(c, ClientOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Proto(rfb_proto::ProtoError::NoAcceptableSecurity)
        ),
        "{err}"
    );
}

#[tokio::test]
async fn a_server_cannot_be_asked_for_plain_credentials_without_tls() {
    let (_c, s) = duplex(64);
    let verifier = PlainVerifier::new(|_, _| true);
    let r = ServerSession::accept(
        s,
        ServerOptions::new(ServerAuth::Plain(verifier), 1, 1, b"x"),
    )
    .await;
    assert!(matches!(r, Err(Error::Config(_))));
}

#[tokio::test(start_paused = true)]
async fn a_stalled_tls_handshake_times_out() {
    // The peer completes the RFB negotiation, then goes silent instead of doing TLS.
    let (c, mut s) = duplex(4096);
    tokio::spawn(async move {
        s.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 12];
        s.read_exact(&mut buf).await.unwrap(); // client version
        s.write_all(&[1, 19]).await.unwrap(); // VeNCrypt only
        let mut one = [0u8; 1];
        s.read_exact(&mut one).await.unwrap(); // chosen type
        s.write_all(&[0, 2, 0, 1, 0, 0, 1, 4]).await.unwrap(); // 0.2, ack, X509None
        let mut rest = [0u8; 6];
        s.read_exact(&mut rest).await.unwrap(); // version + subtype
        s.write_all(&[1]).await.unwrap(); // go: start TLS
        tokio::time::sleep(Duration::from_secs(3600)).await; // never answer the TLS hello
    });
    let id = identity("localhost");
    let mut opts = client_opts(vec![fingerprint(&id.der)]);
    opts.handshake_timeout = Duration::from_secs(5);
    assert!(matches!(
        ClientSession::connect(c, opts).await.unwrap_err(),
        Error::HandshakeTimeout
    ));
}

/// A TCP relay that records what crosses it in both directions.
async fn recording_relay(server_addr: String) -> (String, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let record = Arc::new(Mutex::new(Vec::new()));
    let rec = record.clone();
    tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let server = TcpStream::connect(server_addr).await.unwrap();
        let (mut cr, mut cw) = client.into_split();
        let (mut sr, mut sw) = server.into_split();
        let rec2 = rec.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = cr.read(&mut buf).await {
                if n == 0 || sw.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                rec2.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let mut buf = [0u8; 4096];
        while let Ok(n) = sr.read(&mut buf).await {
            if n == 0 || cw.write_all(&buf[..n]).await.is_err() {
                break;
            }
            rec.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    (addr, record)
}

/// Whether `needle` occurs in `hay`.
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn the_wire_shows_plaintext_without_tls_and_nothing_readable_with_it() {
    const SECRET: &[u8] = b"CLIPBOARD-SECRET-9137";
    const PASSWORD_MARKER: &[u8] = b"RFB 003.008"; // visible in the clear before TLS starts
    let id = identity("localhost");

    for tls in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap().to_string();
        let (relay, record) = recording_relay(server_addr).await;

        let opts = if tls {
            server_opts(&id, ServerAuth::VncPassword(b"hunter2".to_vec()))
        } else {
            ServerOptions::new(ServerAuth::None, 64, 48, b"wire test")
        };
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut session = ServerSession::accept(sock, opts).await.unwrap();
            session.next_message().await.unwrap()
        });

        let sock = TcpStream::connect(&relay).await.unwrap();
        let copts = if tls {
            ClientOptions {
                password: Some(b"hunter2".to_vec()),
                ..client_opts(vec![fingerprint(&id.der)])
            }
        } else {
            ClientOptions::default()
        };
        let (mut client, _) = ClientSession::connect(sock, copts).await.unwrap();
        client
            .send(&ClientMessage::ClientCutText(SECRET.to_vec()))
            .await
            .unwrap();
        // The server got it intact either way.
        assert_eq!(
            server.await.unwrap(),
            ClientMessage::ClientCutText(SECRET.to_vec())
        );

        let wire = record.lock().unwrap().clone();
        assert!(
            contains(&wire, PASSWORD_MARKER),
            "the RFB version exchange is always in the clear"
        );
        if tls {
            assert!(
                !contains(&wire, SECRET),
                "the clipboard text crossed the wire unencrypted"
            );
            assert!(
                !contains(&wire, b"hunter2"),
                "the password crossed the wire"
            );
            assert!(
                !contains(&wire, b"wire test"),
                "the desktop name crossed the wire"
            );
            assert!(
                !contains(&wire, b"tls test"),
                "the desktop name crossed the wire"
            );
        } else {
            assert!(
                contains(&wire, SECRET),
                "without TLS the text is visible, which is the point of TLS"
            );
        }
    }
}
