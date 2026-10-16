//! The WebSocket listener, with clients that behave like noVNC: a browser
//! opens `ws://` or `wss://`, then speaks RFB in binary frames.

use std::{io, sync::Arc, time::Duration};

use rcgen::{CertificateParams, KeyPair};
use rfb_proto::consts::{security, vencrypt};
use rfb_server::{NoInput, Server, Settings, TestPattern};
use rfb_tokio::{
    ClientOptions, ClientSession, PlainVerifier, ServerAuth, ServerOptions,
    ws::{self, OriginPolicy},
};
use rustls::{
    ServerConfig,
    crypto::ring,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn server(auth: ServerAuth) -> Arc<Server> {
    Arc::new(
        Server::new(
            TestPattern::new(64, 48),
            NoInput,
            ServerOptions::new(auth, 1, 1, b"ws test"),
            Settings::default(),
        )
        .unwrap(),
    )
}

/// A self-signed certificate for `localhost`: the server's config and the DER.
fn identity() -> (Arc<ServerConfig>, Vec<u8>) {
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let der = cert.der().to_vec();
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
    (Arc::new(config), der)
}

async fn listen_ws(
    server: &Arc<Server>,
    tls: Option<Arc<ServerConfig>>,
    origins: OriginPolicy,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    server.set_websocket(tls, origins);
    tokio::spawn(Arc::clone(server).listen_ws(listener));
    addr
}

async fn tls_client(
    addr: std::net::SocketAddr,
    der: &[u8],
) -> tokio_rustls::client::TlsStream<TcpStream> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(der.to_vec())).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = TcpStream::connect(addr).await.unwrap();
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_websocket_client_signs_in_with_the_shared_password_over_plain_ws() {
    let server = server(ServerAuth::VncPassword(b"hunter2".to_vec()));
    let addr = listen_ws(&server, None, OriginPolicy::default()).await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let socket = ws::connect(tcp, &addr.to_string(), Some(&format!("http://{addr}")))
        .await
        .unwrap();
    let (mut session, info) = ClientSession::connect(
        socket,
        ClientOptions {
            password: Some(b"hunter2".to_vec()),
            ..ClientOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!((info.width, info.height), (64, 48));
    assert_eq!(info.name, b"ws test");
    // A picture arrives.
    session
        .send(
            &rfb_proto::msg::client::ClientMessage::FramebufferUpdateRequest {
                incremental: false,
                rect: rfb_proto::types::Rect {
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 48,
                },
            },
        )
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, rfb_proto::conn::ClientConnEvent::UpdateBegin { .. }),
        "{event:?}"
    );
    assert_eq!(server.clients().len(), 1);
    assert_eq!(
        server.clients()[0].encryption,
        "none",
        "plain ws is not protected"
    );

    // A wrong password is refused.
    let tcp = TcpStream::connect(addr).await.unwrap();
    let socket = ws::connect(tcp, &addr.to_string(), None).await.unwrap();
    let wrong = ClientSession::connect(
        socket,
        ClientOptions {
            password: Some(b"nope".to_vec()),
            ..ClientOptions::default()
        },
    )
    .await;
    assert!(wrong.is_err());
}

/// Read exactly `n` bytes.
async fn take<R: AsyncRead + Unpin>(r: &mut R, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await.unwrap();
    buf
}

/// What noVNC does for `VeNCrypt` `Plain`, written from the specification.
/// Returns the security result (0 means signed in).
async fn novnc_plain_login<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    user: &[u8],
    password: &[u8],
) -> u32 {
    assert_eq!(take(s, 12).await, b"RFB 003.008\n");
    s.write_all(b"RFB 003.008\n").await.unwrap();
    let count = take(s, 1).await[0];
    let types = take(s, usize::from(count)).await;
    assert!(types.contains(&security::VENCRYPT), "{types:?}");
    assert!(
        !types.contains(&security::NONE),
        "signing in is required: {types:?}"
    );
    s.write_all(&[security::VENCRYPT]).await.unwrap();
    assert_eq!(take(s, 2).await, [0, 2], "VeNCrypt 0.2");
    s.write_all(&[0, 2]).await.unwrap();
    assert_eq!(take(s, 1).await, [0], "version accepted");
    let n = take(s, 1).await[0];
    let mut subtypes = Vec::new();
    for _ in 0..n {
        subtypes.push(u32::from_be_bytes(take(s, 4).await.try_into().unwrap()));
    }
    assert_eq!(subtypes, [vencrypt::PLAIN], "{subtypes:?}");
    s.write_all(&vencrypt::PLAIN.to_be_bytes()).await.unwrap();
    // No acknowledgement byte for Plain: the credentials go straight out.
    let mut creds = Vec::new();
    creds.extend_from_slice(&u32::try_from(user.len()).unwrap().to_be_bytes());
    creds.extend_from_slice(&u32::try_from(password.len()).unwrap().to_be_bytes());
    creds.extend_from_slice(user);
    creds.extend_from_slice(password);
    s.write_all(&creds).await.unwrap();
    u32::from_be_bytes(take(s, 4).await.try_into().unwrap())
}

#[tokio::test]
async fn a_browser_style_client_signs_in_with_user_and_password_over_wss() {
    let (tls, der) = identity();
    let verifier = PlainVerifier::new(|u, p| u == b"alice" && p == b"s3cret");
    let server = server(ServerAuth::Plain(verifier));
    let addr = listen_ws(&server, Some(tls), OriginPolicy::default()).await;

    let tls_stream = tls_client(addr, &der).await;
    let mut socket = ws::connect(tls_stream, "localhost", Some("https://localhost"))
        .await
        .unwrap();
    assert_eq!(novnc_plain_login(&mut socket, b"alice", b"s3cret").await, 0);
    // ClientInit, then ServerInit with the screen size and name.
    socket.write_all(&[1]).await.unwrap();
    let init = take(&mut socket, 4 + 16 + 4).await;
    assert_eq!(u16::from_be_bytes([init[0], init[1]]), 64);
    assert_eq!(u16::from_be_bytes([init[2], init[3]]), 48);
    let name_len = u32::from_be_bytes(init[20..24].try_into().unwrap()) as usize;
    assert_eq!(take(&mut socket, name_len).await, b"ws test");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let clients = server.clients();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].user.as_deref(), Some("alice"));
    assert_eq!(clients[0].encryption, "TLS", "wss is protected");

    // Wrong password: the server says so and hangs up.
    let tls_stream = tls_client(addr, &der).await;
    let mut socket = ws::connect(tls_stream, "localhost", None).await.unwrap();
    assert_ne!(novnc_plain_login(&mut socket, b"alice", b"wrong").await, 0);
}

#[tokio::test]
async fn plain_credentials_are_never_offered_without_the_outer_tls() {
    // The same user accounts over ws:// (no TLS): VeNCrypt Plain must not
    // appear, so there is nothing a browser could send a password through.
    let verifier = PlainVerifier::new(|_, _| true);
    let server = server(ServerAuth::Plain(verifier));
    let addr = listen_ws(&server, None, OriginPolicy::default()).await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let mut socket = ws::connect(tcp, &addr.to_string(), None).await.unwrap();
    // The server cannot offer anything: the handshake ends (a configuration
    // error is logged) before any security type is announced.
    let mut banner = [0u8; 12];
    let first = socket.read_exact(&mut banner).await;
    if first.is_ok() {
        socket.write_all(b"RFB 003.008\n").await.unwrap();
        let mut rest = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut rest)).await;
        assert!(
            !rest.windows(4).any(|w| w == vencrypt::PLAIN.to_be_bytes()),
            "Plain offered without TLS: {rest:?}"
        );
    }
}

#[tokio::test]
async fn a_web_page_from_another_origin_is_turned_away() {
    let server = server(ServerAuth::None);
    let addr = listen_ws(&server, None, OriginPolicy::default()).await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let result = ws::connect(tcp, &addr.to_string(), Some("https://evil.example")).await;
    let err = result.expect_err("refused");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    // The same page may come in when it is listed.
    let allowed = listen_ws(
        &server,
        None,
        OriginPolicy::List(vec!["https://evil.example".into()]),
    )
    .await;
    let tcp = TcpStream::connect(allowed).await.unwrap();
    assert!(
        ws::connect(tcp, &allowed.to_string(), Some("https://evil.example"))
            .await
            .is_ok()
    );
}
