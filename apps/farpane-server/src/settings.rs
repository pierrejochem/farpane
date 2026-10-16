//! `server.toml`: the settings the dashboard's Security page edits and the
//! server reads at start. Command-line options override what is saved here.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::users::write_private;

const VERSION: u32 = 1;

/// How the connection is encrypted with a certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// No TLS.
    #[default]
    Off,
    /// A self-signed certificate kept in the config folder.
    SelfSigned,
}

/// What can be saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettings {
    pub version: u32,
    /// Address and port to listen on, such as `0.0.0.0:5900`.
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub tls: TlsMode,
    /// Offer RSA-AES.
    #[serde(default)]
    pub ra2: bool,
    /// Drop input from every client.
    #[serde(default)]
    pub view_only: bool,
    /// Hold every new client until the operator allows it.
    #[serde(default)]
    pub require_approval: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            version: VERSION,
            listen: None,
            tls: TlsMode::Off,
            ra2: false,
            view_only: false,
            require_approval: false,
        }
    }
}

/// Why the file could not be used.
#[derive(Debug)]
pub enum SettingsError {
    Io(io::Error),
    Format(String),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Format(e) => write!(f, "server.toml: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl ServerSettings {
    /// Read `path`; a missing file gives the defaults.
    ///
    /// # Errors
    /// A damaged file or one from a newer version is an error and is never
    /// overwritten.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let s: Self =
                    toml::from_str(&text).map_err(|e| SettingsError::Format(e.to_string()))?;
                if s.version > VERSION {
                    return Err(SettingsError::Format(format!(
                        "version {} is newer than this program understands ({VERSION})",
                        s.version
                    )));
                }
                Ok(s)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(SettingsError::Io(e)),
        }
    }

    /// Write `path` atomically.
    ///
    /// # Errors
    /// I/O errors.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let text =
            toml::to_string_pretty(self).map_err(|e| SettingsError::Format(e.to_string()))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(SettingsError::Io)?;
        }
        write_private(path, text.as_bytes()).map_err(SettingsError::Io)
    }
}

/// Delete the self-signed certificate and its key: a new pair is made at the
/// next start (viewers then have to trust the new fingerprint).
///
/// # Errors
/// I/O errors other than "not found".
pub fn forget_certificate(config_dir: &Path) -> io::Result<()> {
    remove_if_present(&config_dir.join("tls").join("cert.pem"))?;
    remove_if_present(&config_dir.join("tls").join("key.pem"))
}

/// Delete the RSA-AES key: a new one is made at the next start.
///
/// # Errors
/// I/O errors other than "not found".
pub fn forget_ra2_key(config_dir: &Path) -> io::Result<()> {
    remove_if_present(&config_dir.join("ra2_key.pem"))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_a_missing_file_is_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        assert_eq!(
            ServerSettings::load(&path).unwrap(),
            ServerSettings::default()
        );
        let s = ServerSettings {
            listen: Some("0.0.0.0:5901".into()),
            tls: TlsMode::SelfSigned,
            ra2: true,
            view_only: true,
            ..ServerSettings::default()
        };
        s.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("tls = \"self-signed\""), "{text}");
        assert_eq!(ServerSettings::load(&path).unwrap(), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn damaged_and_newer_files_are_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        fs::write(&path, "tls = [").unwrap();
        assert!(matches!(
            ServerSettings::load(&path),
            Err(SettingsError::Format(_))
        ));
        fs::write(&path, "version = 9\n").unwrap();
        assert!(
            ServerSettings::load(&path)
                .unwrap_err()
                .to_string()
                .contains("newer")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "version = 9\n");
        // Unknown tls values are errors, not silent defaults.
        fs::write(&path, "version = 1\ntls = \"plaintext\"\n").unwrap();
        assert!(ServerSettings::load(&path).is_err());
    }

    #[test]
    fn forgetting_keys_removes_only_them() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["localhost".to_owned()];
        crate::tlsconf::load_or_generate(&dir.path().join("tls"), &names).unwrap();
        crate::tlsconf::load_or_generate_ra2(dir.path()).unwrap();
        fs::write(dir.path().join("users.toml"), "version = 1\n").unwrap();
        forget_certificate(dir.path()).unwrap();
        forget_ra2_key(dir.path()).unwrap();
        assert!(!dir.path().join("tls/cert.pem").exists());
        assert!(!dir.path().join("tls/key.pem").exists());
        assert!(!dir.path().join("ra2_key.pem").exists());
        assert!(dir.path().join("users.toml").exists());
        // Nothing to forget is fine.
        forget_certificate(dir.path()).unwrap();
    }
}
