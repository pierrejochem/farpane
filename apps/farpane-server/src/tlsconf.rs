//! The server's TLS identity: a certificate and key from PEM files, or a
//! self-signed one generated once and kept in the config folder (clients pin
//! its fingerprint on first use).

use std::{fs, io, path::Path, sync::Arc};

use rcgen::{CertificateParams, KeyPair};
use rfb_security::tls::{Fingerprint, fingerprint};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};

use crate::users::write_private;

/// A ready server configuration and the SHA-256 fingerprint of its leaf
/// certificate, which is what a viewer asks its user to confirm.
#[derive(Debug, Clone)]
pub struct Identity {
    pub config: Arc<ServerConfig>,
    pub fingerprint: Fingerprint,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Build an [`Identity`] from PEM files.
///
/// # Errors
/// Unreadable or invalid files, or a key that does not match the certificate.
pub fn load(cert_path: &Path, key_path: &Path) -> io::Result<Identity> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
        .map_err(|e| bad(format!("{}: {e}", cert_path.display())))?
        .collect::<Result<_, _>>()
        .map_err(|e| bad(format!("{}: {e}", cert_path.display())))?;
    let leaf = certs
        .first()
        .ok_or_else(|| bad(format!("{}: no certificate found", cert_path.display())))?;
    let fp = fingerprint(leaf);
    let key = PrivateKeyDer::from_pem_file(key_path)
        .map_err(|e| bad(format!("{}: {e}", key_path.display())))?;
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| bad(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| bad(format!("certificate and key do not fit together: {e}")))?;
    Ok(Identity {
        config: Arc::new(config),
        fingerprint: fp,
    })
}

/// Use `dir/cert.pem` and `dir/key.pem`, generating a self-signed pair for
/// `names` first if they do not exist. The key file is private to its owner.
///
/// # Errors
/// I/O errors, or files that exist but are invalid (they are never replaced).
pub fn load_or_generate(dir: &Path, names: &[String]) -> io::Result<Identity> {
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => return load(&cert_path, &key_path),
        (false, false) => {}
        _ => {
            return Err(bad(format!(
                "only one of {} and {} exists; remove it or restore the other",
                cert_path.display(),
                key_path.display()
            )));
        }
    }
    let mut params = CertificateParams::new(names.to_vec()).map_err(|e| bad(e.to_string()))?;
    // Pinned by fingerprint, so a long life is fine.
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2054, 1, 1);
    let key = KeyPair::generate().map_err(|e| bad(e.to_string()))?;
    let cert = params.self_signed(&key).map_err(|e| bad(e.to_string()))?;
    fs::create_dir_all(dir)?;
    // The key first, so a crash cannot leave a certificate without one.
    write_private(&key_path, key.serialize_pem().as_bytes())?;
    write_private(&cert_path, cert.pem().as_bytes())?;
    load(&cert_path, &key_path)
}

/// The server's RSA key for the RSA-AES security types: `dir/ra2_key.pem`,
/// generated once (it takes a moment) and kept private to its owner. Clients
/// pin the fingerprint of its public half, which stays the same.
///
/// # Errors
/// I/O errors, or a file that exists but is not a usable key (never replaced).
pub fn load_or_generate_ra2(dir: &Path) -> io::Result<Arc<rfb_security::ra2::KeyPair>> {
    use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
    let path = dir.join("ra2_key.pem");
    let pair = match fs::read_to_string(&path) {
        Ok(pem) => {
            let private = rsa::RsaPrivateKey::from_pkcs8_pem(&pem)
                .map_err(|e| bad(format!("{}: {e}", path.display())))?;
            rfb_security::ra2::KeyPair::from_private(private)
                .map_err(|e| bad(format!("{}: {e}", path.display())))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let pair = rfb_security::ra2::KeyPair::generate().map_err(|e| bad(e.to_string()))?;
            let pem = pair
                .private()
                .to_pkcs8_pem(LineEnding::LF)
                .map_err(|e| bad(e.to_string()))?;
            fs::create_dir_all(dir)?;
            write_private(&path, pem.as_bytes())?;
            pair
        }
        Err(e) => return Err(e),
    };
    Ok(Arc::new(pair))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_identity_is_reused_not_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["localhost".to_owned(), "farpane.example".to_owned()];
        let first = load_or_generate(dir.path(), &names).unwrap();
        let second = load_or_generate(dir.path(), &names).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join("key.pem"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the key is private");
        }
    }

    #[test]
    fn a_lone_file_or_a_mismatched_pair_is_an_error_and_nothing_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["localhost".to_owned()];
        load_or_generate(dir.path(), &names).unwrap();
        fs::remove_file(dir.path().join("key.pem")).unwrap();
        assert!(load_or_generate(dir.path(), &names).is_err());
        assert!(
            !dir.path().join("key.pem").exists(),
            "not regenerated behind the user's back"
        );

        // A certificate with somebody else's key.
        let other = tempfile::tempdir().unwrap();
        load_or_generate(other.path(), &names).unwrap();
        let err = load(&dir.path().join("cert.pem"), &other.path().join("key.pem")).unwrap_err();
        assert!(err.to_string().contains("do not fit"), "{err}");
    }

    #[test]
    fn garbage_is_reported_with_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("c.pem");
        let key = dir.path().join("k.pem");
        fs::write(&cert, "not a certificate").unwrap();
        fs::write(&key, "not a key").unwrap();
        let err = load(&cert, &key).unwrap_err().to_string();
        assert!(err.contains("c.pem"), "{err}");
    }
}
