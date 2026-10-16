//! The dashboard window, driven through its accessibility tree.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use egui::{Vec2, accesskit::Role as NodeRole};
use egui_kittest::{Harness, kittest::Queryable};
use farpane_server::{
    backend::{Backend, EventLog, Level, Status},
    dashboard::{Dashboard, Page},
    users::{Role, UserStore},
};
use rfb_server::{ClientInfo, Preview};

#[derive(Debug, Default)]
struct Calls {
    disconnected: Vec<u64>,
    view_only: Vec<(u64, bool)>,
    paused: Vec<bool>,
    decided: Vec<(u64, rfb_server::Decision)>,
}

#[derive(Debug)]
struct Fake {
    clients: Mutex<Vec<ClientInfo>>,
    calls: Mutex<Calls>,
    failure: Mutex<Option<String>>,
    pending: Mutex<Vec<rfb_server::PendingRequest>>,
}

impl Backend for Fake {
    fn status(&self) -> Status {
        Status {
            listen: "127.0.0.1:5900".into(),
            source: "test pattern".into(),
            access: "user accounts, TLS".into(),
            fingerprints: vec![("Certificate".into(), "AA:BB:CC".into())],
            failure: self.failure.lock().unwrap().clone(),
            screen: (320, 200),
            paused: self
                .calls
                .lock()
                .unwrap()
                .paused
                .last()
                .copied()
                .unwrap_or(false),
            offered: vec![
                ("VeNCrypt over TLS".into(), "Certificate".into(), true),
                ("No authentication".into(), "Anyone gets in".into(), false),
            ],
        }
    }

    fn pending(&self) -> Vec<rfb_server::PendingRequest> {
        self.pending.lock().unwrap().clone()
    }

    fn decide(&self, id: u64, decision: rfb_server::Decision) -> bool {
        self.calls.lock().unwrap().decided.push((id, decision));
        self.pending.lock().unwrap().retain(|p| p.id != id);
        true
    }

    fn set_paused(&self, paused: bool) {
        self.calls.lock().unwrap().paused.push(paused);
    }

    fn clients(&self) -> Vec<ClientInfo> {
        self.clients.lock().unwrap().clone()
    }

    fn disconnect(&self, id: u64) -> bool {
        self.calls.lock().unwrap().disconnected.push(id);
        true
    }

    fn set_view_only(&self, id: u64, view_only: bool) -> bool {
        self.calls.lock().unwrap().view_only.push((id, view_only));
        true
    }

    fn preview(&self, _max_edge: u16) -> Preview {
        Preview {
            width: 2,
            height: 2,
            rgba: vec![255; 16],
        }
    }
}

fn client(id: u64, user: Option<&str>) -> ClientInfo {
    ClientInfo {
        id,
        peer: Some(SocketAddr::from((
            [192, 0, 2, 7],
            40_000 + u16::try_from(id).unwrap(),
        ))),
        user: user.map(str::to_owned),
        connected_for: Duration::from_secs(125),
        encryption: "TLS",
        view_only: false,
        bytes_in: 2_000,
        bytes_out: 3_500_000,
        updates: 4,
        encoding: 16,
        rtt: Some(Duration::from_millis(42)),
    }
}

struct Rig {
    fake: Arc<Fake>,
    log: EventLog,
    dir: tempfile::TempDir,
}

fn rig(clients: Vec<ClientInfo>) -> (Rig, Harness<'static, Dashboard>) {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        clients: Mutex::new(clients),
        calls: Mutex::default(),
        failure: Mutex::new(None),
        pending: Mutex::new(Vec::new()),
    });
    let log = EventLog::new();
    let dashboard = Dashboard::new(
        fake.clone(),
        log.clone(),
        dir.path().join("users.toml"),
        dir.path().to_owned(),
    );
    let harness = Harness::builder()
        .with_size(Vec2::new(1100.0, 760.0))
        .build_state(|ctx, d: &mut Dashboard| d.show(ctx), dashboard);
    (Rig { fake, log, dir }, harness)
}

fn settle(h: &mut Harness<'static, Dashboard>) {
    // The window repaints on a timer, so run a fixed number of frames.
    h.run_steps(6);
}

#[test]
fn the_overview_shows_status_clients_traffic_and_fingerprints() {
    let (_rig, mut h) = rig(vec![client(1, Some("alice")), client(2, None)]);
    settle(&mut h);
    h.get_by_label("Sharing");
    h.get_by_label("127.0.0.1:5900");
    h.get_by_label("2");
    h.get_by_label("test pattern (320 × 200)");
    h.get_by_label("user accounts, TLS");
    h.get_by_label("AA:BB:CC");
    h.get_by_label("Clients (2)");
    assert_eq!(h.state().page(), Page::Overview);
}

#[test]
fn a_stopped_capture_is_shown_as_stopped_with_the_reason() {
    let (rig, mut h) = rig(vec![]);
    *rig.fake.failure.lock().unwrap() = Some("the screen went away".into());
    h.run_steps(8);
    h.get_by_label("Stopped");
    h.get_by_label("Capture stopped: the screen went away");
}

#[test]
fn the_clients_page_lists_clients_and_can_disconnect_or_restrict_them() {
    let (rig, mut h) = rig(vec![client(1, Some("alice")), client(2, None)]);
    settle(&mut h);
    h.get_by_label("Clients (2)").click();
    settle(&mut h);
    assert_eq!(h.state().page(), Page::Clients);
    h.get_by_label("alice (192.0.2.7:40001)");
    h.get_by_label("192.0.2.7:40002");
    assert_eq!(h.get_all_by_label("ZRLE").count(), 2);
    assert_eq!(h.get_all_by_label("2 min").count(), 2);
    assert_eq!(h.get_all_by_label("42 ms").count(), 2);
    h.get_all_by_label("Disconnect").next().unwrap().click();
    settle(&mut h);
    assert_eq!(rig.fake.calls.lock().unwrap().disconnected, [1]);
    h.get_all_by_label("View only").nth(1).unwrap().click();
    settle(&mut h);
    assert_eq!(rig.fake.calls.lock().unwrap().view_only, [(2, true)]);
    // Both actions are in the event log.
    let lines: Vec<String> = rig.log.lines().into_iter().map(|l| l.text).collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("alice") && l.contains("disconnected")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("view only")), "{lines:?}");
}

#[test]
fn an_empty_clients_page_says_so() {
    let (_rig, mut h) = rig(vec![]);
    h.state_mut().set_page(Page::Clients);
    settle(&mut h);
    h.get_by_label("Nobody is connected.");
}

#[test]
fn the_users_page_adds_changes_and_removes_users_through_the_same_rules_as_the_cli() {
    let (rig, mut h) = rig(vec![]);
    h.state_mut().set_page(Page::Users);
    settle(&mut h);
    h.get_by_label_contains("No users yet");
    // Fill the form: user name, display name, password (in this order).
    // The first box is the top bar's search.
    let boxes: Vec<_> = h.get_all_by_role(NodeRole::TextInput).skip(1).collect();
    assert_eq!(boxes.len(), 2);
    boxes[0].focus();
    boxes[0].type_text("boss");
    boxes[1].focus();
    boxes[1].type_text("The Boss");
    let password = h.get_by_role(NodeRole::PasswordInput);
    password.focus();
    password.type_text("a long password");
    h.run_steps(2);
    h.get_by_label("Add user").click();
    settle(&mut h);
    h.get_by_label("added boss (control)");
    let store = UserStore::load(&rig.dir.path().join("users.toml")).unwrap();
    let boss = store.get("boss").expect("the user was saved");
    assert_eq!(
        (boss.display_name.as_str(), boss.role),
        ("The Boss", Role::Control)
    );
    assert!(store.verify("boss", b"a long password").is_some());
    // The form is empty again and the list shows the user.
    h.run_steps(30);
    h.get_by_label("boss");
    h.get_by_label("The Boss");
    // A short password is refused with the CLI's message.
    let boxes: Vec<_> = h.get_all_by_role(NodeRole::TextInput).skip(1).collect();
    boxes[0].focus();
    boxes[0].type_text("second");
    let password = h.get_by_role(NodeRole::PasswordInput);
    password.focus();
    password.type_text("short");
    h.run_steps(2);
    h.get_by_label("Add user").click();
    settle(&mut h);
    h.get_by_label_contains("at least 8");
    // Remove the user.
    h.get_by_label("Remove boss").click();
    settle(&mut h);
    h.get_by_label("boss removed");
    assert!(
        UserStore::load(&rig.dir.path().join("users.toml"))
            .unwrap()
            .get("boss")
            .is_none()
    );
}

#[test]
fn the_last_admin_cannot_be_removed_from_the_dashboard_either() {
    let (rig, mut h) = rig(vec![]);
    let mut store = UserStore::load(&rig.dir.path().join("users.toml")).unwrap();
    store
        .add("root-user", "", b"admin password", Role::Admin)
        .unwrap();
    store.save().unwrap();
    h.state_mut().set_page(Page::Users);
    h.run_steps(30);
    h.get_by_label("Remove root-user").click();
    settle(&mut h);
    h.get_by_label_contains("last enabled admin");
    assert!(
        UserStore::load(&rig.dir.path().join("users.toml"))
            .unwrap()
            .get("root-user")
            .is_some()
    );
}

#[test]
fn a_broken_users_file_is_reported_not_overwritten() {
    let (rig, mut h) = rig(vec![]);
    std::fs::write(rig.dir.path().join("users.toml"), "garbage [").unwrap();
    h.state_mut().set_page(Page::Users);
    h.run_steps(30);
    h.get_by_label_contains("The users file cannot be read");
    assert_eq!(
        std::fs::read_to_string(rig.dir.path().join("users.toml")).unwrap(),
        "garbage ["
    );
}

#[test]
fn the_log_page_shows_lines_newest_first_and_exports_json() {
    let (rig, mut h) = rig(vec![]);
    rig.log.push(Level::Info, "127.0.0.1:1 connected");
    rig.log.push(Level::Warning, "sign-in failed for \"bob\"");
    h.state_mut().set_page(Page::Log);
    settle(&mut h);
    h.get_by_label("127.0.0.1:1 connected");
    h.get_by_label("sign-in failed for \"bob\"");
    h.get_by_label("Export as JSON").click();
    settle(&mut h);
    let json = std::fs::read_to_string(rig.dir.path().join("event-log.json")).unwrap();
    assert!(json.contains("sign-in failed for \\\"bob\\\""), "{json}");
    assert!(
        h.state()
            .notice()
            .is_some_and(|n| n.contains("event-log.json"))
    );
    h.get_by_label("Clear").click();
    settle(&mut h);
    h.get_by_label("Nothing has happened yet.");
    assert_eq!(rig.log.lines().len(), 0);
}

#[test]
fn traffic_rates_come_from_the_byte_counters_between_samples() {
    let (rig, mut h) = rig(vec![client(1, None)]);
    settle(&mut h);
    assert_eq!(
        h.state().rates(),
        (0.0, 0.0),
        "the first sample has nothing to compare"
    );
    // Another second of traffic: 1000 bytes in, 500000 out.
    {
        let mut c = rig.fake.clients.lock().unwrap();
        c[0].bytes_in += 1_000;
        c[0].bytes_out += 500_000;
    }
    // Frames are about 10 ms apart in the harness; run until a sample is taken.
    let mut seen = (0.0, 0.0);
    for _ in 0..200 {
        h.step();
        let r = h.state().rates();
        if r.1 > 0.0 {
            seen = r;
            break;
        }
    }
    assert!(seen.0 > 0.0 && seen.1 > 0.0, "{seen:?}");
    assert!(
        (seen.1 / seen.0 - 500.0).abs() < 1.0,
        "out/in is 500: {seen:?}"
    );
}

#[test]
fn an_invalid_listen_address_is_refused_and_nothing_is_saved() {
    let (rig, mut h) = rig(vec![]);
    h.state_mut().set_page(Page::Security);
    settle(&mut h);
    h.get_by_label("user accounts, TLS");
    h.get_by_label_contains("Always on");
    assert!(
        !rig.dir.path().join("server.toml").exists(),
        "nothing saved yet"
    );
    let listen = h.get_all_by_role(NodeRole::TextInput).nth(1).unwrap();
    listen.focus();
    listen.type_text("not an address");
    h.run_steps(2);
    h.get_by_label("Save settings").click();
    settle(&mut h);
    h.get_by_label_contains("is not an address and port");
    assert!(!rig.dir.path().join("server.toml").exists());
}

#[test]
fn the_security_page_saves_what_was_ticked_and_typed() {
    use farpane_server::settings::{ServerSettings, TlsMode};
    let (rig, mut h) = rig(vec![]);
    h.state_mut().set_page(Page::Security);
    settle(&mut h);
    h.get_by_label("Encrypt with a self-signed certificate (TLS)")
        .click();
    h.get_by_label("Offer RSA-AES (needs users)").click();
    let listen = h.get_all_by_role(NodeRole::TextInput).nth(1).unwrap();
    listen.focus();
    listen.type_text("127.0.0.1:5999");
    h.run_steps(2);
    h.get_by_label("Save settings").click();
    settle(&mut h);
    let saved = ServerSettings::load(&rig.dir.path().join("server.toml")).unwrap();
    assert_eq!(saved.listen.as_deref(), Some("127.0.0.1:5999"));
    assert_eq!(saved.tls, TlsMode::SelfSigned);
    assert!(saved.ra2 && !saved.view_only);
}

#[test]
fn saved_settings_appear_again_and_a_valid_address_is_stored() {
    use farpane_server::settings::{ServerSettings, TlsMode};
    let (rig, mut h) = rig(vec![]);
    ServerSettings {
        listen: Some("127.0.0.1:5999".into()),
        tls: TlsMode::SelfSigned,
        ra2: true,
        ..ServerSettings::default()
    }
    .save(&rig.dir.path().join("server.toml"))
    .unwrap();
    h.state_mut().set_page(Page::Security);
    settle(&mut h);
    // Turn view-only on and save: the other saved values stay.
    h.get_by_label("View only for everyone").click();
    h.get_by_label("Save settings").click();
    settle(&mut h);
    h.get_by_label("Saved. Restart the server to apply.");
    let saved = ServerSettings::load(&rig.dir.path().join("server.toml")).unwrap();
    assert_eq!(saved.listen.as_deref(), Some("127.0.0.1:5999"));
    assert_eq!(saved.tls, TlsMode::SelfSigned);
    assert!(saved.ra2 && saved.view_only);
    assert!(
        rig.log
            .lines()
            .iter()
            .any(|l| l.text.contains("security settings saved"))
    );
}

#[test]
fn forgetting_a_key_asks_first_and_removes_only_that_key() {
    let (rig, mut h) = rig(vec![]);
    let names = vec!["localhost".to_owned()];
    farpane_server::tlsconf::load_or_generate(&rig.dir.path().join("tls"), &names).unwrap();
    farpane_server::tlsconf::load_or_generate_ra2(rig.dir.path()).unwrap();
    h.state_mut().set_page(Page::Security);
    settle(&mut h);
    h.get_by_label("Make a new certificate").click();
    settle(&mut h);
    h.get_by_label_contains("Viewers that trusted the old one will warn.");
    // Cancel: nothing happens.
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert!(rig.dir.path().join("tls/cert.pem").exists());
    // Confirm: the certificate goes, the RSA-AES key stays.
    h.get_by_label("Make a new certificate").click();
    settle(&mut h);
    h.get_by_label("Yes, forget it").click();
    settle(&mut h);
    assert!(!rig.dir.path().join("tls/cert.pem").exists());
    assert!(!rig.dir.path().join("tls/key.pem").exists());
    assert!(rig.dir.path().join("ra2_key.pem").exists());
    h.get_by_label_contains("The certificate was removed");
    assert!(
        rig.log
            .lines()
            .iter()
            .any(|l| l.level == Level::Warning && l.text.contains("certificate was forgotten"))
    );
}

#[test]
fn a_broken_settings_file_is_reported_not_overwritten() {
    let (rig, mut h) = rig(vec![]);
    std::fs::write(rig.dir.path().join("server.toml"), "tls = [").unwrap();
    h.state_mut().set_page(Page::Security);
    settle(&mut h);
    h.get_by_label_contains("The settings file cannot be read");
    assert_eq!(
        std::fs::read_to_string(rig.dir.path().join("server.toml")).unwrap(),
        "tls = ["
    );
}

#[test]
fn listening_beyond_this_computer_without_users_is_flagged() {
    use farpane_server::settings::ServerSettings;
    let (rig, mut h) = rig(vec![]);
    ServerSettings {
        listen: Some("0.0.0.0:5900".into()),
        ..ServerSettings::default()
    }
    .save(&rig.dir.path().join("server.toml"))
    .unwrap();
    h.state_mut().set_page(Page::Security);
    settle(&mut h);
    h.get_by_label_contains("Reachable from other machines but no users exist");
}

#[test]
fn pause_sharing_asks_the_server_and_the_button_turns_into_resume() {
    let (rig, mut h) = rig(vec![]);
    settle(&mut h);
    h.get_by_label("Sharing");
    h.get_by_label("Pause sharing").click();
    settle(&mut h);
    assert_eq!(rig.fake.calls.lock().unwrap().paused, [true]);
    h.get_by_label("Paused");
    h.get_by_label("Resume sharing").click();
    settle(&mut h);
    assert_eq!(rig.fake.calls.lock().unwrap().paused, [true, false]);
    let lines: Vec<String> = rig.log.lines().into_iter().map(|l| l.text).collect();
    assert!(lines.iter().any(|l| l.contains("paused")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("resumed")), "{lines:?}");
}

#[test]
fn the_search_box_filters_clients_and_events() {
    let (rig, mut h) = rig(vec![client(1, Some("alice")), client(2, Some("bob"))]);
    rig.log.push(Level::Info, "carol connected");
    rig.log.push(Level::Info, "dave connected");
    settle(&mut h);
    h.get_by_label("alice (192.0.2.7:40001)");
    h.get_by_label("bob (192.0.2.7:40002)");
    h.get_by_label("carol connected");
    let search = h.get_by_role(NodeRole::TextInput);
    search.focus();
    search.type_text("alice");
    settle(&mut h);
    h.get_by_label("alice (192.0.2.7:40001)");
    assert!(h.query_by_label("bob (192.0.2.7:40002)").is_none());
    assert!(h.query_by_label("carol connected").is_none());
    h.get_by_role(NodeRole::TextInput).type_text("zzz");
    settle(&mut h);
    h.get_by_label("No client matches the search.");
}

#[test]
fn the_overview_lists_the_offered_sign_in_methods_and_links_to_the_security_page() {
    let (_rig, mut h) = rig(vec![]);
    settle(&mut h);
    h.get_by_label("VeNCrypt over TLS");
    h.get_by_label("On");
    h.get_by_label("Off");
    h.get_by_label("Change on the Security page").click();
    settle(&mut h);
    assert_eq!(h.state().page(), Page::Security);
}

#[test]
fn the_displays_page_shows_what_is_shared() {
    let (_rig, mut h) = rig(vec![]);
    h.state_mut().set_page(Page::Displays);
    settle(&mut h);
    h.get_by_label("test pattern");
    h.get_by_label("320 × 200");
}

#[test]
fn a_waiting_client_gets_an_approval_card_and_the_answer_goes_to_the_server() {
    let (rig, mut h) = rig(vec![]);
    rig.fake
        .pending
        .lock()
        .unwrap()
        .push(rfb_server::PendingRequest {
            id: 7,
            peer: Some(SocketAddr::from(([192, 0, 2, 44], 50_000))),
            user: Some("anna".into()),
            encryption: "RSA-AES",
            waiting_for: Duration::from_secs(3),
        });
    settle(&mut h);
    h.get_by_label("Connection request");
    h.get_by_label_contains("anna from 192.0.2.44:50000");
    h.get_by_label("Allow view only").click();
    settle(&mut h);
    assert_eq!(
        rig.fake.calls.lock().unwrap().decided,
        [(7, rfb_server::Decision::ViewOnly)]
    );
    assert!(h.query_by_label("Connection request").is_none(), "answered");
    let lines: Vec<String> = rig.log.lines().into_iter().map(|l| l.text).collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("anna") && l.contains("view only")),
        "{lines:?}"
    );
}
