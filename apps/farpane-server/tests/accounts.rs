//! Users, roles, lockout and TLS through a running server.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use farpane_server::{
    config::{self, TlsChoice},
    users::{Role, UserStore},
};
use rfb_proto::msg::client::ClientMessage;
use rfb_security::tls::Fingerprint;
use rfb_server::{Input, Server, Settings, TestPattern};
use rfb_tokio::{ClientOptions, ClientSession, Error, TlsOptions};
use tokio::io::duplex;

#[derive(Debug, Clone, Default)]
struct Recording(Arc<Mutex<Vec<String>>>);

impl Input for Recording {
    fn key(&self, keysym: u32, down: bool) {
        self.0
            .lock()
            .unwrap()
            .push(format!("key {keysym:x} {down}"));
    }
    fn pointer(&self, _: u16, _: u16, _: u8) {}
    fn clipboard(&self, _: &str) {}
}

struct Setup {
    _dir: tempfile::TempDir,
    server: Arc<Server>,
    pin: Fingerprint,
    input: Recording,
    log: Arc<Mutex<Vec<String>>>,
}

fn setup(tls: bool) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let mut users = UserStore::load(&dir.path().join("users.toml")).unwrap();
    users
        .add("admin", "", b"admin password", Role::Admin)
        .unwrap();
    users
        .add("watcher", "", b"watcher password", Role::View)
        .unwrap();
    users.save().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let choice = if tls {
        TlsChoice::SelfSigned {
            names: vec!["localhost".to_owned()],
        }
    } else {
        TlsChoice::Off
    };
    let access = config::choose(dir.path(), None, &choice, false, move |l| {
        sink.lock().unwrap().push(l);
    });
    let access = match access {
        Ok(a) => a,
        Err(e) => panic!("{e}"),
    };
    let input = Recording::default();
    let server = Server::new(
        TestPattern::new(64, 64),
        input.clone(),
        access.template.clone(),
        Settings::default(),
    )
    .unwrap();
    let auth = access.authenticator.clone().unwrap();
    server.set_policy(move |u| auth.permission(u));
    Setup {
        pin: access.identity.as_ref().map_or([0; 32], |i| i.fingerprint),
        _dir: dir,
        server: Arc::new(server),
        input,
        log,
    }
}

fn client(user: &str, password: &str, pin: Fingerprint) -> ClientOptions {
    ClientOptions {
        username: Some(user.as_bytes().to_vec()),
        password: Some(password.as_bytes().to_vec()),
        tls: Some(TlsOptions {
            server_name: "localhost".to_owned(),
            pins: vec![pin],
            extra_roots: Vec::new(),
        }),
        ..ClientOptions::default()
    }
}

async fn sign_in(
    s: &Setup,
    options: ClientOptions,
) -> Result<ClientSession<tokio::io::DuplexStream>, Error> {
    let (near, far) = duplex(1 << 20);
    let server = Arc::clone(&s.server);
    tokio::spawn(async move { server.serve_connection(far).await });
    ClientSession::connect(near, options)
        .await
        .map(|(session, _)| session)
}

#[test]
fn users_without_tls_are_refused_with_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let mut users = UserStore::load(&dir.path().join("users.toml")).unwrap();
    users
        .add("admin", "", b"admin password", Role::Admin)
        .unwrap();
    users.save().unwrap();
    let err = config::choose(dir.path(), None, &TlsChoice::Off, false, |_| {}).unwrap_err();
    assert!(
        err.contains("needs encryption")
            && err.contains("--tls-self-signed")
            && err.contains("--ra2"),
        "{err}"
    );
}

#[test]
fn without_users_the_shared_password_or_nothing_applies() {
    let dir = tempfile::tempdir().unwrap();
    let a = config::choose(
        dir.path(),
        Some(b"pw".to_vec()),
        &TlsChoice::Off,
        false,
        |_| {},
    )
    .unwrap();
    assert!(a.description.starts_with("shared password") && a.authenticator.is_none());
    let b = config::choose(dir.path(), None, &TlsChoice::Off, false, |_| {}).unwrap();
    assert!(b.description.starts_with("no authentication"));
}

#[tokio::test]
async fn admins_control_and_view_users_only_look() {
    let s = setup(true);
    for (user, password, expect_input) in [
        ("admin", "admin password", true),
        ("watcher", "watcher password", false),
    ] {
        s.input.0.lock().unwrap().clear();
        let mut session = sign_in(&s, client(user, password, s.pin)).await.unwrap();
        assert!(session.is_encrypted(), "sign-in happens inside TLS");
        session
            .send(&ClientMessage::KeyEvent {
                down: true,
                keysym: 0x61,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let seen = s.input.0.lock().unwrap().clone();
        // The previous user's session ends with a key release; only presses
        // show that input got through.
        let pressed = seen.iter().any(|e| e.ends_with("true"));
        assert_eq!(pressed, expect_input, "{user}: {seen:?}");
    }
}

#[tokio::test]
async fn a_wrong_password_fails_and_five_failures_lock_the_account() {
    let s = setup(true);
    for _ in 0..5 {
        let err = sign_in(&s, client("admin", "not the password", s.pin))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("authentication failed"), "{err}");
    }
    // Locked: even the right password is refused now.
    assert!(
        sign_in(&s, client("admin", "admin password", s.pin))
            .await
            .is_err()
    );
    // Another account is unaffected.
    assert!(
        sign_in(&s, client("watcher", "watcher password", s.pin))
            .await
            .is_ok()
    );
    let log = s.log.lock().unwrap().join("\n");
    assert!(log.contains("locked for 15 minutes"), "{log}");
    assert!(
        !log.contains("not the password") && !log.contains("admin password"),
        "{log}"
    );
}

#[tokio::test]
async fn the_certificate_must_match_the_pinned_fingerprint() {
    let s = setup(true);
    let err = sign_in(&s, client("admin", "admin password", [7; 32]))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Untrusted { .. }), "{err}");
}

#[tokio::test]
async fn users_can_sign_in_over_rsa_aes_without_tls_and_roles_still_apply() {
    use rfb_tokio::Ra2Options;
    let dir = tempfile::tempdir().unwrap();
    let mut users = UserStore::load(&dir.path().join("users.toml")).unwrap();
    users
        .add("admin", "", b"admin password", Role::Admin)
        .unwrap();
    users
        .add("watcher", "", b"watcher password", Role::View)
        .unwrap();
    users.save().unwrap();
    let access = config::choose(dir.path(), None, &TlsChoice::Off, true, |_| {}).unwrap();
    assert!(access.identity.is_none());
    let fingerprint = access.ra2_fingerprint.expect("the RSA-AES key was made");
    assert!(
        access.description.contains("RSA-AES"),
        "{}",
        access.description
    );
    // The key is kept: asking again gives the same fingerprint.
    let again = config::choose(dir.path(), None, &TlsChoice::Off, true, |_| {}).unwrap();
    assert_eq!(again.ra2_fingerprint, Some(fingerprint));

    let input = Recording::default();
    let server = Server::new(
        TestPattern::new(64, 64),
        input.clone(),
        access.template.clone(),
        Settings::default(),
    )
    .unwrap();
    let auth = access.authenticator.clone().unwrap();
    server.set_policy(move |u| auth.permission(u));
    let server = Arc::new(server);
    for (user, password, expect_input) in [
        ("admin", "admin password", true),
        ("watcher", "watcher password", false),
    ] {
        input.0.lock().unwrap().clear();
        let (near, far) = duplex(1 << 20);
        let s = Arc::clone(&server);
        tokio::spawn(async move { s.serve_connection(far).await });
        let options = ClientOptions {
            username: Some(user.as_bytes().to_vec()),
            password: Some(password.as_bytes().to_vec()),
            ra2: Some(Ra2Options {
                server_name: "localhost".to_owned(),
                pins: vec![fingerprint],
            }),
            ..ClientOptions::default()
        };
        let (mut session, _) = ClientSession::connect(near, options).await.unwrap();
        assert!(session.is_ra2() && !session.is_encrypted());
        session
            .send(&ClientMessage::KeyEvent {
                down: true,
                keysym: 0x61,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let seen = input.0.lock().unwrap().clone();
        // The previous user's session ends with a key release; only presses
        // show that input got through.
        let pressed = seen.iter().any(|e| e.ends_with("true"));
        assert_eq!(pressed, expect_input, "{user}: {seen:?}");
    }
}
