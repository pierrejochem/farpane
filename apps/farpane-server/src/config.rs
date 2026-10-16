//! Turning command-line choices into the server's authentication and TLS.

use std::path::{Path, PathBuf};

use rfb_tokio::{ServerAuth, ServerOptions};

use crate::{
    auth::Authenticator,
    tlsconf::{self, Identity},
    users::UserStore,
};

/// Where the TLS identity comes from.
#[derive(Debug, Clone)]
pub enum TlsChoice {
    /// No TLS: only for loopback, or inside a tunnel.
    Off,
    /// A self-signed pair kept in the config folder, made for `names`.
    SelfSigned { names: Vec<String> },
    /// PEM files.
    Files { cert: PathBuf, key: PathBuf },
}

/// Everything decided about signing in.
#[derive(Debug)]
pub struct Access {
    pub template: ServerOptions,
    /// `Some` when users sign in by name: asks for their role.
    pub authenticator: Option<Authenticator>,
    pub identity: Option<Identity>,
    /// One line for the log.
    pub description: String,
}

/// The default config folder: `/etc/farpane` if it exists, else the user's.
#[must_use]
pub fn default_config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FARPANE_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let system = PathBuf::from("/etc/farpane");
    if system.is_dir() {
        return system;
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("farpane");
    }
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from("farpane"),
        |h| PathBuf::from(h).join(".config/farpane"),
    )
}

/// Decide how clients sign in.
///
/// With enabled users in `users.toml` they sign in by name and password
/// (`X509Plain`), which needs TLS. Otherwise the shared `password` (VNC
/// Auth, or `X509Vnc` under TLS), or nothing.
///
/// # Errors
/// A message for the user: users but no TLS, unreadable users file or
/// certificate.
pub fn choose(
    config_dir: &Path,
    password: Option<Vec<u8>>,
    tls: &TlsChoice,
    log: impl Fn(String) + Send + Sync + 'static,
) -> Result<Access, String> {
    let users = UserStore::load(&config_dir.join("users.toml")).map_err(|e| e.to_string())?;
    let identity = match tls {
        TlsChoice::Off => None,
        TlsChoice::SelfSigned { names } => Some(
            tlsconf::load_or_generate(&config_dir.join("tls"), names)
                .map_err(|e| format!("TLS certificate: {e}"))?,
        ),
        TlsChoice::Files { cert, key } => {
            Some(tlsconf::load(cert, key).map_err(|e| format!("TLS certificate: {e}"))?)
        }
    };
    let (auth, authenticator, what) = if users.has_enabled_users() {
        if identity.is_none() {
            return Err(
                "users are defined, and signing in by name needs TLS so the password is \
                 encrypted: pass --tls-self-signed (or --tls-cert and --tls-key)"
                    .to_owned(),
            );
        }
        let authenticator = Authenticator::new(users, log);
        (
            ServerAuth::Plain(authenticator.verifier()),
            Some(authenticator),
            "user accounts",
        )
    } else if let Some(p) = password {
        (ServerAuth::VncPassword(p), None, "shared password")
    } else {
        (ServerAuth::None, None, "no authentication")
    };
    let mut template = ServerOptions::new(auth, 1, 1, b"Farpane");
    if let Some(id) = &identity {
        template = template.with_tls(id.config.clone());
    }
    let description = format!(
        "{what}, {}",
        if identity.is_some() { "TLS" } else { "no TLS" }
    );
    Ok(Access {
        template,
        authenticator,
        identity,
        description,
    })
}
