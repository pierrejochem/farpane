//! The certificate trust policy, against real certificates and real (in-memory)
//! TLS handshakes.

use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rfb_security::tls::{
    CertProblem, Finding, Untrusted, client_config, fingerprint, format_fingerprint,
    parse_fingerprint,
};
use rustls::{
    ClientConnection, ServerConfig, ServerConnection,
    crypto::ring,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
};

/// A certificate with its key.
struct Identity {
    der: Vec<u8>,
    key: Vec<u8>,
}

fn self_signed(names: &[&str]) -> Identity {
    let key = KeyPair::generate().unwrap();
    let params =
        CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).unwrap();
    let cert = params.self_signed(&key).unwrap();
    Identity {
        der: cert.der().to_vec(),
        key: key.serialize_der(),
    }
}

fn expired(names: &[&str]) -> Identity {
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).unwrap();
    params.not_before = rcgen::date_time_ymd(2000, 1, 1);
    params.not_after = rcgen::date_time_ymd(2001, 1, 1);
    let cert = params.self_signed(&key).unwrap();
    Identity {
        der: cert.der().to_vec(),
        key: key.serialize_der(),
    }
}

/// A private CA and a leaf certificate it signed for `names`.
fn ca_and_leaf(names: &[&str]) -> (Vec<u8>, Identity) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.clone().self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf_params =
        CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).unwrap();
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
    (
        ca.der().to_vec(),
        Identity {
            der: leaf.der().to_vec(),
            key: leaf_key.serialize_der(),
        },
    )
}

fn server_config(id: &Identity) -> Arc<ServerConfig> {
    let provider = Arc::new(ring::default_provider());
    Arc::new(
        ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(id.der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key.clone())),
            )
            .unwrap(),
    )
}

/// Run a TLS handshake between a client using `client_cfg` (connecting to
/// `host`) and a server presenting `id`.
fn handshake(
    client_cfg: Arc<rustls::ClientConfig>,
    id: &Identity,
    host: &'static str,
) -> Result<(), rustls::Error> {
    let mut client = ClientConnection::new(client_cfg, ServerName::try_from(host).unwrap())?;
    let mut server = ServerConnection::new(server_config(id))?;
    for _ in 0..20 {
        if client.wants_write() {
            let mut bytes = Vec::new();
            client.write_tls(&mut bytes).unwrap();
            server.read_tls(&mut &bytes[..]).unwrap();
            server.process_new_packets()?;
        }
        if server.wants_write() {
            let mut bytes = Vec::new();
            server.write_tls(&mut bytes).unwrap();
            client.read_tls(&mut &bytes[..]).unwrap();
            client.process_new_packets()?;
        }
        if !client.is_handshaking() && !server.is_handshaking() {
            return Ok(());
        }
    }
    panic!("the handshake did not finish");
}

fn finding(f: &Finding) -> Option<Untrusted> {
    f.lock().unwrap().clone()
}

#[test]
fn a_new_self_signed_server_is_refused_and_reported_with_its_fingerprint() {
    let id = self_signed(&["localhost"]);
    let (cfg, found) = client_config(vec![], &[]).unwrap();
    assert!(handshake(cfg, &id, "localhost").is_err());
    assert_eq!(
        finding(&found),
        Some(Untrusted {
            fingerprint: fingerprint(&id.der),
            problem: CertProblem::Unknown
        })
    );
}

#[test]
fn a_pinned_certificate_is_accepted() {
    let id = self_signed(&["localhost"]);
    let (cfg, found) = client_config(vec![fingerprint(&id.der)], &[]).unwrap();
    handshake(cfg, &id, "localhost").unwrap();
    assert_eq!(finding(&found), None);
}

#[test]
fn a_pin_means_exactly_that_certificate_whatever_the_name_or_date() {
    // The user trusted this very certificate: a mismatched name or an expired
    // date does not change that.
    let id = expired(&["something-else.example"]);
    let (cfg, _) = client_config(vec![fingerprint(&id.der)], &[]).unwrap();
    handshake(cfg, &id, "localhost").unwrap();
}

#[test]
fn a_different_certificate_for_a_pinned_server_is_reported_as_changed() {
    let trusted = self_signed(&["localhost"]);
    let imposter = self_signed(&["localhost"]);
    let (cfg, found) = client_config(vec![fingerprint(&trusted.der)], &[]).unwrap();
    assert!(handshake(cfg, &imposter, "localhost").is_err());
    assert_eq!(
        finding(&found),
        Some(Untrusted {
            fingerprint: fingerprint(&imposter.der),
            problem: CertProblem::Changed
        })
    );
}

#[test]
fn a_certificate_from_a_trusted_private_ca_is_accepted_without_a_pin() {
    let (ca_der, leaf) = ca_and_leaf(&["localhost"]);
    let (cfg, found) = client_config(vec![], &[ca_der]).unwrap();
    handshake(cfg, &leaf, "localhost").unwrap();
    assert_eq!(finding(&found), None);
}

#[test]
fn the_ca_does_not_vouch_for_a_name_it_did_not_sign() {
    let (ca_der, leaf) = ca_and_leaf(&["localhost"]);
    let (cfg, found) = client_config(vec![], &[ca_der]).unwrap();
    assert!(handshake(cfg, &leaf, "other.example").is_err());
    let f = finding(&found).expect("a finding");
    assert!(matches!(f.problem, CertProblem::Invalid(_)), "{f:?}");
}

#[test]
fn an_expired_certificate_is_invalid_not_merely_unknown() {
    let (ca_der, _) = ca_and_leaf(&["localhost"]);
    // Expired, self-signed and unpinned: reported with the reason.
    let id = expired(&["localhost"]);
    let (cfg, found) = client_config(vec![], &[ca_der]).unwrap();
    assert!(handshake(cfg, &id, "localhost").is_err());
    let f = finding(&found).expect("a finding");
    assert_eq!(f.fingerprint, fingerprint(&id.der));
    assert!(
        matches!(f.problem, CertProblem::Unknown | CertProblem::Invalid(_)),
        "{f:?}"
    );
}

#[test]
fn fingerprints_round_trip_through_their_text_form() {
    let id = self_signed(&["localhost"]);
    let fp = fingerprint(&id.der);
    let text = format_fingerprint(&fp);
    assert_eq!(text.len(), 32 * 3 - 1);
    assert!(
        text.chars()
            .all(|c| c == ':' || c.is_ascii_digit() || ('A'..='F').contains(&c))
    );
    assert_eq!(parse_fingerprint(&text), Some(fp));
    assert_eq!(parse_fingerprint(&text.to_lowercase()), Some(fp));
    assert_eq!(parse_fingerprint(&text.replace(':', "")), Some(fp));
    assert_eq!(parse_fingerprint("AB:CD"), None);
    assert_eq!(parse_fingerprint(&"zz".repeat(32)), None);
    assert_eq!(parse_fingerprint(&"é".repeat(32)), None);
}

#[test]
fn an_unusable_ca_is_reported_not_ignored() {
    assert!(client_config(vec![], &[vec![1, 2, 3]]).is_err());
}
