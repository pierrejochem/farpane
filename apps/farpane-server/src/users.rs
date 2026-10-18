//! The server's own user list (design doc, section 20): names, Argon2id
//! password hashes and roles in `users.toml`, readable only by its owner.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier},
};
use serde::{Deserialize, Serialize};

/// What a user may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Everything, including managing users and settings.
    Admin,
    /// View and control the desktop.
    Control,
    /// Look only.
    View,
}

impl Role {
    /// Parse `admin`, `control` or `view`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "admin" => Some(Self::Admin),
            "control" => Some(Self::Control),
            "view" => Some(Self::View),
            _ => None,
        }
    }

    /// May this role send keyboard, pointer and clipboard input?
    #[must_use]
    pub fn may_control(self) -> bool {
        !matches!(self, Self::View)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Admin => "admin",
            Self::Control => "control",
            Self::View => "view",
        })
    }
}

/// One account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    /// A PHC string, `$argon2id$...`.
    pub password_hash: String,
    pub role: Role,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Seconds since 1970-01-01.
    #[serde(default)]
    pub created: u64,
}

fn yes() -> bool {
    true
}

/// The file format version this build writes and understands.
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default, rename = "user")]
    users: Vec<User>,
}

/// Why a user operation failed.
#[derive(Debug)]
pub enum UserError {
    Io(io::Error),
    /// The file is not valid, or from a newer version.
    Format(String),
    BadName(&'static str),
    BadPassword(&'static str),
    Exists(String),
    NoSuchUser(String),
    Hash(String),
}

impl fmt::Display for UserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Format(e) => write!(f, "users file: {e}"),
            Self::BadName(why) => write!(f, "user name {why}"),
            Self::BadPassword(why) => write!(f, "password {why}"),
            Self::Exists(n) => write!(f, "user {n:?} already exists"),
            Self::NoSuchUser(n) => write!(f, "no user {n:?}"),
            Self::Hash(e) => write!(f, "hashing failed: {e}"),
        }
    }
}

impl std::error::Error for UserError {}

impl From<io::Error> for UserError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

const MIN_PASSWORD: usize = 8;
/// 255 so that every password also fits the one-byte length of RSA-AES.
const MAX_PASSWORD: usize = 255;

/// The users, with the file they live in.
#[derive(Debug)]
pub struct UserStore {
    path: PathBuf,
    users: Vec<User>,
    /// Modification time and length when last read: a same-second edit that
    /// changes the length is still noticed.
    modified: Option<(SystemTime, u64)>,
}

/// Check a new account's name and password against the rules `add` applies,
/// without hashing or saving anything (for forms that want to say what is
/// wrong before asking for more).
///
/// # Errors
/// [`UserError::BadName`] or [`UserError::BadPassword`].
pub fn validate_new(name: &str, password: &[u8]) -> Result<(), UserError> {
    valid_name(name)?;
    check_password(password)
}

fn valid_name(name: &str) -> Result<(), UserError> {
    if name.is_empty() || name.len() > 64 {
        return Err(UserError::BadName("must be 1 to 64 characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
    {
        return Err(UserError::BadName(
            "may only contain letters, digits and . _ - @",
        ));
    }
    Ok(())
}

fn check_password(password: &[u8]) -> Result<(), UserError> {
    if password.len() < MIN_PASSWORD {
        return Err(UserError::BadPassword("must be at least 8 characters"));
    }
    if password.len() > MAX_PASSWORD {
        return Err(UserError::BadPassword("must be at most 255 bytes"));
    }
    Ok(())
}

fn hash(password: &[u8]) -> Result<String, UserError> {
    check_password(password)?;
    Argon2::default()
        .hash_password(password)
        .map(|h| h.to_string())
        .map_err(|e| UserError::Hash(e.to_string()))
}

impl UserStore {
    /// Read `path`; a missing file is an empty list.
    ///
    /// # Errors
    /// I/O errors and [`UserError::Format`]. A damaged file is never
    /// overwritten: fix or move it by hand.
    pub fn load(path: &Path) -> Result<Self, UserError> {
        let (users, modified) = match fs::read_to_string(path) {
            Ok(text) => {
                let file: File =
                    toml::from_str(&text).map_err(|e| UserError::Format(e.to_string()))?;
                if file.version > VERSION {
                    return Err(UserError::Format(format!(
                        "version {} is newer than this program understands ({VERSION})",
                        file.version
                    )));
                }
                (file.users, stamp(path))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (Vec::new(), None),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path: path.to_owned(),
            users,
            modified,
        })
    }

    /// Reload if the file changed on disk since it was read. Returns whether
    /// it did. A file that fails to parse is ignored (the old list stays), so a
    /// half-written edit cannot lock everyone out.
    pub fn reload_if_changed(&mut self) -> bool {
        let now = stamp(&self.path);
        if now == self.modified {
            return false;
        }
        match Self::load(&self.path) {
            Ok(fresh) => {
                *self = fresh;
                true
            }
            Err(_) => false,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn users(&self) -> &[User] {
        &self.users
    }

    /// Whether anyone can sign in.
    #[must_use]
    pub fn has_enabled_users(&self) -> bool {
        self.users.iter().any(|u| u.enabled)
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.users
            .iter()
            .position(|u| u.name.eq_ignore_ascii_case(name))
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&User> {
        self.find(name).map(|i| &self.users[i])
    }

    /// Write the file: atomically, readable only by its owner.
    ///
    /// # Errors
    /// I/O errors.
    pub fn save(&mut self) -> Result<(), UserError> {
        let text = toml::to_string_pretty(&File {
            version: VERSION,
            users: self.users.clone(),
        })
        .map_err(|e| UserError::Format(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        write_private(&self.path, text.as_bytes())?;
        self.modified = stamp(&self.path);
        Ok(())
    }

    /// Add a user (not saved until [`UserStore::save`]).
    ///
    /// # Errors
    /// Bad name or password, or the name is taken.
    pub fn add(
        &mut self,
        name: &str,
        display_name: &str,
        password: &[u8],
        role: Role,
    ) -> Result<(), UserError> {
        valid_name(name)?;
        if self.find(name).is_some() {
            return Err(UserError::Exists(name.to_owned()));
        }
        let password_hash = hash(password)?;
        self.users.push(User {
            name: name.to_owned(),
            display_name: display_name.to_owned(),
            password_hash,
            role,
            enabled: true,
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        });
        Ok(())
    }

    fn user_mut(&mut self, name: &str) -> Result<&mut User, UserError> {
        let i = self
            .find(name)
            .ok_or_else(|| UserError::NoSuchUser(name.to_owned()))?;
        Ok(&mut self.users[i])
    }

    /// # Errors
    /// No such user, or a bad password.
    pub fn set_password(&mut self, name: &str, password: &[u8]) -> Result<(), UserError> {
        let new = hash(password)?;
        self.user_mut(name)?.password_hash = new;
        Ok(())
    }

    /// # Errors
    /// No such user.
    pub fn set_role(&mut self, name: &str, role: Role) -> Result<(), UserError> {
        self.user_mut(name)?.role = role;
        Ok(())
    }

    /// # Errors
    /// No such user.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), UserError> {
        self.user_mut(name)?.enabled = enabled;
        Ok(())
    }

    /// # Errors
    /// No such user.
    pub fn remove(&mut self, name: &str) -> Result<(), UserError> {
        let i = self
            .find(name)
            .ok_or_else(|| UserError::NoSuchUser(name.to_owned()))?;
        self.users.remove(i);
        Ok(())
    }

    /// Check a sign-in. Takes about as long for an unknown or disabled user
    /// as for a wrong password, so timing does not reveal which names exist.
    #[must_use]
    pub fn verify(&self, name: &str, password: &[u8]) -> Option<Role> {
        let user = self.get(name).filter(|u| u.enabled);
        let stored: &str = match user {
            Some(u) => &u.password_hash,
            None => dummy_hash(),
        };
        let ok = Argon2::default().verify_password(password, stored).is_ok();
        user.filter(|_| ok).map(|u| u.role)
    }
}

/// A valid hash nobody knows the password of, checked when the user does not
/// exist. Computed once.
fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        Argon2::default()
            .hash_password(b"no such user, no such password")
            .map(|h| h.to_string())
            .unwrap_or_default()
    })
}

/// Write `data` to `path` through a temporary file, with mode 0600 on Unix.
/// Modification time and length of `path`, if it can be read.
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

pub(crate) fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    // `create_new` refuses a leftover file or a planted symlink, which
    // would keep its own (possibly wider) permissions.
    let _ = fs::remove_file(&tmp);
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, UserStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = UserStore::load(&dir.path().join("users.toml")).unwrap();
        (dir, s)
    }

    #[test]
    fn passwords_are_argon2id_hashes_that_verify() {
        let (_d, mut s) = store();
        s.add("alice", "Alice A.", b"correct horse", Role::Admin)
            .unwrap();
        let u = s.get("alice").unwrap();
        assert!(
            u.password_hash.starts_with("$argon2id$"),
            "{}",
            u.password_hash
        );
        assert!(!u.password_hash.contains("correct horse"));
        assert_eq!(s.verify("alice", b"correct horse"), Some(Role::Admin));
        assert_eq!(
            s.verify("ALICE", b"correct horse"),
            Some(Role::Admin),
            "names ignore case"
        );
        assert_eq!(s.verify("alice", b"wrong horse!"), None);
        assert_eq!(s.verify("bob", b"correct horse"), None);
    }

    #[test]
    fn two_users_with_the_same_password_get_different_hashes() {
        let (_d, mut s) = store();
        s.add("a", "", b"same password", Role::View).unwrap();
        s.add("b", "", b"same password", Role::View).unwrap();
        assert_ne!(
            s.get("a").unwrap().password_hash,
            s.get("b").unwrap().password_hash
        );
    }

    #[test]
    fn names_and_passwords_are_validated() {
        let (_d, mut s) = store();
        for bad in ["", "has space", "slash/name", "new\nline", &"x".repeat(65)] {
            assert!(
                matches!(
                    s.add(bad, "", b"long enough", Role::View),
                    Err(UserError::BadName(_))
                ),
                "{bad:?}"
            );
        }
        assert!(matches!(
            s.add("ok", "", b"short", Role::View),
            Err(UserError::BadPassword(_))
        ));
        assert!(matches!(
            s.add("ok", "", &[b'x'; 257], Role::View),
            Err(UserError::BadPassword(_))
        ));
        s.add("ok", "", b"long enough", Role::View).unwrap();
        assert!(matches!(
            s.add("OK", "", b"long enough", Role::View),
            Err(UserError::Exists(_))
        ));
    }

    #[test]
    fn disabled_users_cannot_sign_in_and_roles_can_change() {
        let (_d, mut s) = store();
        s.add("carol", "", b"pass word 1", Role::Control).unwrap();
        s.set_enabled("carol", false).unwrap();
        assert_eq!(s.verify("carol", b"pass word 1"), None);
        assert!(!s.has_enabled_users());
        s.set_enabled("carol", true).unwrap();
        s.set_role("carol", Role::View).unwrap();
        assert_eq!(s.verify("carol", b"pass word 1"), Some(Role::View));
        s.set_password("carol", b"pass word 2").unwrap();
        assert_eq!(s.verify("carol", b"pass word 1"), None);
        assert_eq!(s.verify("carol", b"pass word 2"), Some(Role::View));
        s.remove("carol").unwrap();
        assert!(matches!(s.remove("carol"), Err(UserError::NoSuchUser(_))));
    }

    #[test]
    fn the_file_round_trips_and_is_private() {
        let (dir, mut s) = store();
        s.add("dave", "Dave D.", b"pass word 1", Role::Admin)
            .unwrap();
        s.save().unwrap();
        let path = dir.path().join("users.toml");
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("version = 1") && text.contains("[[user]]"),
            "{text}"
        );
        let again = UserStore::load(&path).unwrap();
        assert_eq!(again.users(), s.users());
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
    fn a_damaged_or_newer_file_is_refused_and_left_alone() {
        let (dir, _s) = store();
        let path = dir.path().join("users.toml");
        fs::write(&path, "this is = not [valid").unwrap();
        assert!(matches!(UserStore::load(&path), Err(UserError::Format(_))));
        fs::write(&path, "version = 99\n").unwrap();
        let err = UserStore::load(&path).unwrap_err().to_string();
        assert!(err.contains("newer"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "version = 99\n");
    }

    #[test]
    fn a_running_server_notices_edits_but_ignores_half_written_files() {
        let (dir, mut s) = store();
        let path = dir.path().join("users.toml");
        s.add("erin", "", b"pass word 1", Role::Control).unwrap();
        s.save().unwrap();
        let mut running = UserStore::load(&path).unwrap();
        assert!(!running.reload_if_changed());
        // Another process adds a user.
        let mut editor = UserStore::load(&path).unwrap();
        editor.add("frank", "", b"pass word 2", Role::View).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        editor.save().unwrap();
        assert!(running.reload_if_changed());
        assert!(running.get("frank").is_some());
        // A broken edit keeps the last good list.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, "garbage [").unwrap();
        let _ = running.reload_if_changed();
        assert!(running.get("frank").is_some());
    }
}
