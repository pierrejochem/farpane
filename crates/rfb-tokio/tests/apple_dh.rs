//! Apple Remote Desktop sign-in (security type 30): the client against a
//! scripted server that does its half with `rfb_security::apple_dh`.

use rfb_proto::{PixelFormat, consts::security};
use rfb_security::apple_dh;
use rfb_tokio::{ClientOptions, ClientSession, Error};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

/// RFC 2409 group 2, generator 2.
const PRIME: &str = "ffffffffffffffffc90fdaa22168c234c4c6628b80dc1cd129024e088a67cc74020bbea63b139b22514a08798e3404ddef9519b3cd3a431b302b0a6df25f14374fe1356d6d51c245e485b576625e7ec6f44c42e9a637ed6b0bff5cb6f406b7edee386bfb5a899fa5ae9f24117c4b1fe649286651ece65381ffffffffffffffff";

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

async fn server_init(s: &mut DuplexStream) {
    let mut init = Vec::new();
    init.extend_from_slice(&1024u16.to_be_bytes());
    init.extend_from_slice(&768u16.to_be_bytes());
    init.extend_from_slice(&PixelFormat::RGB888.to_bytes());
    init.extend_from_slice(&5u32.to_be_bytes());
    init.extend_from_slice(b"a Mac");
    s.write_all(&init).await.unwrap();
}

/// A server that offers `types`; with type 30 it checks the credentials.
/// Returns the security type the client chose and what it signed in with.
async fn server(
    mut s: DuplexStream,
    types: &[u8],
    accept: (&[u8], &[u8]),
) -> (u8, Option<(Vec<u8>, Vec<u8>)>) {
    s.write_all(b"RFB 003.889\n").await.unwrap();
    let mut version = [0u8; 12];
    s.read_exact(&mut version).await.unwrap();
    assert_eq!(&version, b"RFB 003.008\n");
    let mut offer = vec![u8::try_from(types.len()).unwrap()];
    offer.extend_from_slice(types);
    s.write_all(&offer).await.unwrap();
    let mut chosen = [0u8; 1];
    s.read_exact(&mut chosen).await.unwrap();
    let mut signed_in = None;
    if chosen[0] == security::APPLE_DH {
        let prime = hex(PRIME);
        let private: Vec<u8> = (0..128u32)
            .map(|i| u8::try_from((i * 7 + 3) % 256).unwrap())
            .collect();
        let key = apple_dh::public_key(2, &prime, &private).unwrap();
        let mut params = Vec::new();
        params.extend_from_slice(&2u16.to_be_bytes());
        params.extend_from_slice(&128u16.to_be_bytes());
        params.extend_from_slice(&prime);
        params.extend_from_slice(&key);
        s.write_all(&params).await.unwrap();
        let mut credentials = [0u8; 128];
        s.read_exact(&mut credentials).await.unwrap();
        let mut client_key = vec![0u8; 128];
        s.read_exact(&mut client_key).await.unwrap();
        let got = apple_dh::open(2, &prime, &private, &client_key, &credentials).unwrap();
        let ok = (got.0.as_slice(), got.1.as_slice()) == accept;
        signed_in = Some(got);
        if ok {
            s.write_all(&0u32.to_be_bytes()).await.unwrap();
        } else {
            let why = b"wrong user or password";
            s.write_all(&1u32.to_be_bytes()).await.unwrap();
            s.write_all(&u32::try_from(why.len()).unwrap().to_be_bytes())
                .await
                .unwrap();
            s.write_all(why).await.unwrap();
            return (chosen[0], signed_in);
        }
    } else {
        // VNC Auth: a challenge, the response, and an open door.
        s.write_all(&[7u8; 16]).await.unwrap();
        let mut response = [0u8; 16];
        s.read_exact(&mut response).await.unwrap();
        s.write_all(&0u32.to_be_bytes()).await.unwrap();
    }
    let mut shared = [0u8; 1];
    s.read_exact(&mut shared).await.unwrap();
    server_init(&mut s).await;
    (chosen[0], signed_in)
}

fn opts(user: Option<&[u8]>, password: &[u8]) -> ClientOptions {
    ClientOptions {
        username: user.map(<[u8]>::to_vec),
        password: Some(password.to_vec()),
        ..ClientOptions::default()
    }
}

#[tokio::test]
async fn an_account_signs_in_with_apple_dh() {
    let (c, s) = duplex(64 * 1024);
    let server = tokio::spawn(server(
        s,
        &[security::VNC_AUTH, security::APPLE_DH],
        (b"jochem", b"s3cret"),
    ));
    let (session, info) = ClientSession::connect(c, opts(Some(b"jochem"), b"s3cret"))
        .await
        .unwrap();
    assert_eq!(session.security_type(), Some(security::APPLE_DH));
    assert_eq!((info.width, info.height), (1024, 768));
    assert_eq!(info.name, b"a Mac");
    let (chosen, signed_in) = server.await.unwrap();
    assert_eq!(chosen, security::APPLE_DH);
    assert_eq!(signed_in, Some((b"jochem".to_vec(), b"s3cret".to_vec())));
}

#[tokio::test]
async fn a_wrong_password_is_reported_with_the_servers_reason() {
    let (c, s) = duplex(64 * 1024);
    let server = tokio::spawn(server(s, &[security::APPLE_DH], (b"jochem", b"right")));
    let err = ClientSession::connect(c, opts(Some(b"jochem"), b"wrong"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("wrong user or password"), "{err}");
    server.await.unwrap();
}

#[tokio::test]
async fn without_a_user_name_the_plain_password_type_is_used() {
    let (c, s) = duplex(64 * 1024);
    let server = tokio::spawn(server(
        s,
        &[security::APPLE_DH, security::VNC_AUTH],
        (b"", b""),
    ));
    let (session, _) = ClientSession::connect(c, opts(None, b"pw")).await.unwrap();
    assert_eq!(session.security_type(), Some(security::VNC_AUTH));
    assert_eq!(server.await.unwrap(), (security::VNC_AUTH, None));
}

#[tokio::test]
async fn a_server_with_only_apple_dh_needs_a_user_name() {
    let (c, s) = duplex(64 * 1024);
    let server = tokio::spawn(server(s, &[security::APPLE_DH], (b"", b"")));
    let err = ClientSession::connect(c, opts(None, b"pw"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Proto(_)), "{err:?}");
    drop(server);
}
