//! The first-run wizard: what it asks and what it does with the answers.
//!
//! The dashboard draws the steps; this module holds the choices, checks them
//! and writes the result: the first administrator into `users.toml` and the
//! matching `server.toml`. There are no default accounts or passwords: until
//! the wizard (or `farpane-server user add`) creates one, nobody can sign in
//! by name.
//!
//! Encryption is not a choice. An account's password must not cross the
//! network in the clear, and the server refuses to offer accounts without TLS
//! or RSA-AES, so the wizard always turns on TLS with a self-signed
//! certificate (the Security page can swap that for certificate files).

use std::path::Path;

use crate::{
    settings::{ServerSettings, TlsMode},
    users::{Role, UserError, UserStore, validate_new},
};

/// Where viewers may connect from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    /// Only programs on this computer (127.0.0.1).
    #[default]
    ThisComputer,
    /// Other computers on the network (0.0.0.0).
    Network,
}

/// The standard port for this kind of server.
const PORT: u16 = 5900;

/// What the user chose. Passwords are only held while the wizard is open.
#[derive(Clone, Default)]
pub struct Choices {
    pub access: Access,
    pub username: String,
    pub display_name: String,
    pub password: String,
    pub repeat: String,
    /// Also offer RSA-AES next to TLS.
    pub ra2: bool,
    /// Hold every new client until the person at this computer allows it.
    pub require_approval: bool,
}

impl std::fmt::Debug for Choices {
    // Never print the passwords, not even in a failed test.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Choices")
            .field("access", &self.access)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl Choices {
    /// The starting point: this computer only, approval off (the person who
    /// sets it up is sitting there), and approval on as soon as the network
    /// is chosen (see [`Choices::set_access`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose where viewers may connect from. Opening the server to the
    /// network turns approval on, which is the safer starting point; it can
    /// be turned off again.
    pub fn set_access(&mut self, access: Access) {
        if access != self.access {
            self.require_approval = access == Access::Network;
        }
        self.access = access;
    }

    /// What is wrong with the account details, if anything.
    #[must_use]
    pub fn account_problem(&self) -> Option<String> {
        if let Err(e) = validate_new(self.username.trim(), self.password.as_bytes()) {
            return Some(match e {
                UserError::BadName(_) | UserError::BadPassword(_) => format!("The {e}."),
                other => other.to_string(),
            });
        }
        if self.password != self.repeat {
            return Some("The two passwords are not the same.".to_owned());
        }
        None
    }

    /// The settings these choices stand for.
    #[must_use]
    pub fn settings(&self) -> ServerSettings {
        let host = match self.access {
            Access::ThisComputer => "127.0.0.1",
            Access::Network => "0.0.0.0",
        };
        ServerSettings {
            listen: Some(format!("{host}:{PORT}")),
            tls: TlsMode::SelfSigned,
            ra2: self.ra2,
            require_approval: self.require_approval,
            ..ServerSettings::default()
        }
    }

    /// Create the administrator and save the settings.
    ///
    /// The account goes first. If the settings cannot be written afterwards
    /// the account is taken out again, so a failed run leaves the folder as
    /// it was and the wizard can be tried again.
    ///
    /// # Errors
    /// A message for the user: bad details, an unreadable or damaged users
    /// file (which is never overwritten), or a file that cannot be written.
    pub fn apply(&self, users_file: &Path, settings_file: &Path) -> Result<ServerSettings, String> {
        if let Some(problem) = self.account_problem() {
            return Err(problem);
        }
        let name = self.username.trim();
        let mut store = UserStore::load(users_file).map_err(|e| e.to_string())?;
        let display = if self.display_name.trim().is_empty() {
            name
        } else {
            self.display_name.trim()
        };
        store
            .add(name, display, self.password.as_bytes(), Role::Admin)
            .map_err(|e| e.to_string())?;
        store.save().map_err(|e| e.to_string())?;
        let settings = self.settings();
        if let Err(e) = settings.save(settings_file) {
            // Put the users file back as it was.
            if store.remove(name).is_ok() {
                let _ = store.save();
            }
            return Err(format!("Could not save the settings: {e}"));
        }
        Ok(settings)
    }
}

/// Whether this looks like a server nobody has set up: no accounts and no
/// saved settings. A damaged file in either place says someone did set it up,
/// so that is not a first run (and the wizard must not write over it).
#[must_use]
pub fn is_first_run(users_file: &Path, settings_file: &Path) -> bool {
    !settings_file.exists()
        && UserStore::load(users_file).is_ok_and(|store| store.users().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> Choices {
        Choices {
            username: "admin".into(),
            display_name: "Ada Admin".into(),
            password: "correct horse".into(),
            repeat: "correct horse".into(),
            ..Choices::new()
        }
    }

    #[test]
    fn the_choices_become_settings_and_encryption_is_always_on() {
        let mut c = filled();
        let local = c.settings();
        assert_eq!(local.listen.as_deref(), Some("127.0.0.1:5900"));
        assert_eq!(local.tls, TlsMode::SelfSigned);
        assert!(!local.require_approval && !local.ra2);
        c.set_access(Access::Network);
        c.ra2 = true;
        let net = c.settings();
        assert_eq!(net.listen.as_deref(), Some("0.0.0.0:5900"));
        assert_eq!(net.tls, TlsMode::SelfSigned);
        assert!(
            net.require_approval,
            "opening to the network turns approval on"
        );
        assert!(net.ra2);
        // The person may turn approval off again, and going back keeps that choice.
        c.require_approval = false;
        c.set_access(Access::Network);
        assert!(!c.require_approval);
    }

    #[test]
    fn account_details_are_checked_with_the_rules_of_the_user_store() {
        let mut c = filled();
        assert_eq!(c.account_problem(), None);
        c.username = "  ".into();
        assert!(c.account_problem().unwrap().contains("user name"));
        c.username = "bad name!".into();
        assert!(c.account_problem().unwrap().contains("letters, digits"));
        c = filled();
        c.password = "short".into();
        c.repeat = "short".into();
        assert!(c.account_problem().unwrap().contains("at least 8"));
        c = filled();
        c.repeat = "correct hors".into();
        assert!(c.account_problem().unwrap().contains("not the same"));
    }

    #[test]
    fn debug_never_shows_the_password() {
        let text = format!("{:?}", filled());
        assert!(!text.contains("correct horse"), "{text}");
        assert!(text.contains("admin"));
    }

    #[test]
    fn apply_creates_the_first_admin_and_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let (users, settings) = (
            dir.path().join("users.toml"),
            dir.path().join("server.toml"),
        );
        assert!(is_first_run(&users, &settings));
        let saved = filled().apply(&users, &settings).unwrap();
        assert_eq!(ServerSettings::load(&settings).unwrap(), saved);
        let store = UserStore::load(&users).unwrap();
        let admin = store.get("admin").unwrap();
        assert_eq!(admin.role, Role::Admin);
        assert_eq!(admin.display_name, "Ada Admin");
        assert_eq!(store.verify("admin", b"correct horse"), Some(Role::Admin));
        assert_eq!(store.verify("admin", b"wrong"), None);
        // The password is not in either file.
        for file in [&users, &settings] {
            assert!(
                !std::fs::read_to_string(file)
                    .unwrap()
                    .contains("correct horse")
            );
        }
        assert!(!is_first_run(&users, &settings));
    }

    #[test]
    fn a_missing_display_name_falls_back_to_the_user_name() {
        let dir = tempfile::tempdir().unwrap();
        let (users, settings) = (
            dir.path().join("users.toml"),
            dir.path().join("server.toml"),
        );
        let mut c = filled();
        c.display_name = " ".into();
        c.apply(&users, &settings).unwrap();
        assert_eq!(
            UserStore::load(&users)
                .unwrap()
                .get("admin")
                .unwrap()
                .display_name,
            "admin"
        );
    }

    #[test]
    fn bad_details_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (users, settings) = (
            dir.path().join("users.toml"),
            dir.path().join("server.toml"),
        );
        let mut c = filled();
        c.repeat = "different".into();
        assert!(c.apply(&users, &settings).is_err());
        assert!(!users.exists() && !settings.exists());
    }

    #[test]
    fn a_failed_settings_write_takes_the_account_out_again() {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users.toml");
        // A directory where the settings file should go makes writing fail.
        let settings = dir.path().join("server.toml");
        std::fs::create_dir(&settings).unwrap();
        let err = filled().apply(&users, &settings).unwrap_err();
        assert!(err.contains("settings"), "{err}");
        assert_eq!(UserStore::load(&users).unwrap().users().len(), 0);
    }

    #[test]
    fn existing_or_damaged_files_are_not_a_first_run_and_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (users, settings) = (
            dir.path().join("users.toml"),
            dir.path().join("server.toml"),
        );
        std::fs::write(&users, "this is not toml [[[").unwrap();
        assert!(!is_first_run(&users, &settings));
        assert!(filled().apply(&users, &settings).is_err());
        assert_eq!(
            std::fs::read_to_string(&users).unwrap(),
            "this is not toml [[["
        );
        assert!(!settings.exists());
    }

    #[test]
    fn a_name_that_exists_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (users, settings) = (
            dir.path().join("users.toml"),
            dir.path().join("server.toml"),
        );
        filled().apply(&users, &settings).unwrap();
        std::fs::remove_file(&settings).unwrap();
        assert!(
            filled()
                .apply(&users, &settings)
                .unwrap_err()
                .contains("admin")
        );
    }
}
