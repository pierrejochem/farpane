//! RSA-AES (RA2) client and server against each other.

use std::sync::Arc;

use rfb_proto::{
    consts::security,
    handshake::ClientConfig,
    msg::{client::ClientMessage, server::ServerMessage},
};
use rfb_security::ra2::KeyPair;
use rfb_tokio::{
    ClientOptions, ClientSession, Error, PlainVerifier, Ra2Options, Ra2Server, ServerAuth,
    ServerOptions, ServerSession,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

fn key() -> Arc<KeyPair> {
    static KEY: std::sync::OnceLock<Arc<KeyPair>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::new(KeyPair::generate().unwrap())))
}

fn server_opts(unencrypted: bool) -> ServerOptions {
    let verifier = PlainVerifier::new(|u, p| u == b"alice" && p == b"s3cret");
    let mut opts =
        ServerOptions::new(ServerAuth::Plain(verifier), 64, 48, b"ra2 test").with_ra2(key());
    if unencrypted {
        opts.ra2 = Some(Ra2Server {
            key: key(),
            allow_unencrypted: true,
        });
    }
    opts
}

fn client_opts(pins: Vec<[u8; 32]>, password: &str, types: &[u8]) -> ClientOptions {
    ClientOptions {
        handshake: ClientConfig {
            security: types.to_vec(),
            ..ClientConfig::default()
        },
        username: Some(b"alice".to_vec()),
        password: Some(password.as_bytes().to_vec()),
        ra2: Some(Ra2Options {
            server_name: "localhost".to_owned(),
            pins,
        }),
        ..ClientOptions::default()
    }
}

fn pin() -> [u8; 32] {
    key().public().fingerprint()
}

#[tokio::test]
async fn every_supported_type_signs_in_and_carries_traffic() {
    for (types, encrypted) in [
        (vec![security::RA2], true),
        (vec![security::RA2_256], true),
        (vec![security::RA2NE], false),
        (vec![security::RA2NE_256], false),
    ] {
        let (c, s) = duplex(1 << 20);
        let server = tokio::spawn(ServerSession::accept(s, server_opts(true)));
        let (mut client, info) =
            ClientSession::connect(c, client_opts(vec![pin()], "s3cret", &types))
                .await
                .unwrap();
        let mut server = server.await.unwrap().unwrap();
        assert_eq!(
            (info.width, info.height, info.name.as_slice()),
            (64, 48, &b"ra2 test"[..])
        );
        assert_eq!(server.username(), Some(&b"alice"[..]), "{types:?}");
        assert_eq!(client.is_ra2(), encrypted, "{types:?}");
        assert_eq!(server.is_ra2(), encrypted, "{types:?}");
        // Traffic in both directions, including a message larger than one
        // encrypted record (8192 bytes).
        let text = vec![b'x'; 100_000];
        client
            .send(&ClientMessage::ClientCutText(text.clone()))
            .await
            .unwrap_or_else(|e| panic!("{types:?}: {e}"));
        // Read it on the server while the client keeps the pipe moving.
        assert_eq!(
            server.next_message().await.unwrap(),
            ClientMessage::ClientCutText(text)
        );
        server.send(&ServerMessage::Bell).await.unwrap();
        client.next_event().await.unwrap();
    }
}

#[tokio::test]
async fn the_default_preference_picks_rsa_aes_256() {
    let (c, s) = duplex(1 << 16);
    let server = tokio::spawn(ServerSession::accept(s, server_opts(false)));
    let mut opts = client_opts(vec![pin()], "s3cret", &[]);
    opts.handshake = ClientConfig::default();
    let (client, _) = ClientSession::connect(c, opts).await.unwrap();
    server.await.unwrap().unwrap();
    assert!(client.is_ra2());
}

#[tokio::test]
async fn a_wrong_password_is_refused() {
    let (c, s) = duplex(1 << 16);
    let server = tokio::spawn(ServerSession::accept(s, server_opts(false)));
    let err = ClientSession::connect(c, client_opts(vec![pin()], "wrong", &[security::RA2_256]))
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
}

#[tokio::test]
async fn an_unpinned_or_different_server_key_is_reported_not_trusted() {
    // Unknown: nothing pinned yet. The fingerprint comes back for the user.
    let (c, s) = duplex(1 << 16);
    tokio::spawn(ServerSession::accept(s, server_opts(false)));
    let err = ClientSession::connect(c, client_opts(vec![], "s3cret", &[security::RA2_256]))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Untrusted { fingerprint, problem: rfb_security::tls::CertProblem::Unknown, .. } if *fingerprint == pin()),
        "{err}"
    );
    // Changed: another key is pinned.
    let (c, s) = duplex(1 << 16);
    tokio::spawn(ServerSession::accept(s, server_opts(false)));
    let err = ClientSession::connect(
        c,
        client_opts(vec![[9; 32]], "s3cret", &[security::RA2_256]),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Untrusted {
                problem: rfb_security::tls::CertProblem::Changed,
                ..
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn without_ra2_options_or_a_password_the_client_never_picks_rsa_aes() {
    let (c, s) = duplex(1 << 16);
    tokio::spawn(ServerSession::accept(s, server_opts(false)));
    let mut opts = client_opts(vec![pin()], "s3cret", &[]);
    opts.handshake = ClientConfig::default();
    opts.ra2 = None;
    assert!(ClientSession::connect(c, opts).await.is_err());
}

#[tokio::test]
async fn rsa_aes_needs_user_accounts_on_the_server() {
    let opts = ServerOptions::new(ServerAuth::None, 8, 8, b"x").with_ra2(key());
    let (_c, s) = duplex(64);
    let err = ServerSession::accept(s, opts).await.unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
}

/// A relay that damages one byte of the server's output after `after` bytes.
fn tamper(
    server_end: tokio::io::DuplexStream,
    after: usize,
    total: Arc<std::sync::atomic::AtomicUsize>,
) -> tokio::io::DuplexStream {
    let (near, far) = duplex(1 << 16);
    let (mut server_r, mut server_w) = tokio::io::split(server_end);
    let (mut client_r, mut client_w) = tokio::io::split(far);
    tokio::spawn(async move {
        let mut seen = 0usize;
        let mut buf = [0u8; 512];
        while let Ok(n) = server_r.read(&mut buf).await {
            if n == 0 {
                break;
            }
            if seen <= after && after < seen + n {
                buf[after - seen] ^= 0x01;
            }
            seen += n;
            total.store(seen, std::sync::atomic::Ordering::Relaxed);
            if client_w.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_r, &mut server_w).await;
        // Let the server see the client leave.
        let _ = server_w.shutdown().await;
    });
    near
}

async fn run_through_relay(after: usize) -> (bool, usize) {
    let (c, s) = duplex(1 << 16);
    let server = tokio::spawn(async move {
        let mut session = ServerSession::accept(s, server_opts(false)).await?;
        session.send(&ServerMessage::Bell).await?;
        session.send(&ServerMessage::Bell).await?;
        Ok::<_, Error>(session)
    });
    let total = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = tamper(c, after, Arc::clone(&total));
    let mut opts = client_opts(vec![pin()], "s3cret", &[security::RA2_256]);
    // A damaged length field leaves the client waiting for bytes that never
    // come; the timeouts end that.
    opts.handshake_timeout = std::time::Duration::from_millis(600);
    let outcome = async {
        let (mut client, _) = ClientSession::connect(c, opts).await?;
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_millis(600), client.next_event())
                .await
                .map_err(|_| Error::HandshakeTimeout)??;
        }
        Ok::<_, Error>(())
    }
    .await;
    let _ = server.await;
    if let Err(e) = &outcome {
        eprintln!("flip at {after}: {e}");
    }
    (
        outcome.is_ok(),
        total.load(std::sync::atomic::Ordering::Relaxed),
    )
}

#[tokio::test]
async fn a_damaged_message_ends_the_connection_instead_of_being_read() {
    // How much the server sends when nothing is damaged.
    let (ok, sent) = run_through_relay(usize::MAX).await;
    assert!(ok && sent > 600, "{ok} {sent}");
    // Flipping any single byte of it, in the keys, the encrypted random
    // numbers, the handshake messages or the traffic after, must be noticed.
    for after in (0..sent).step_by(37) {
        let (ok, _) = run_through_relay(after).await;
        assert!(
            !ok,
            "a flipped byte at offset {after} of {sent} was accepted"
        );
    }
}
