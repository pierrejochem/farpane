//! Saved settings and the address book: a small TOML file in the platform's
//! configuration directory.
//!
//! Passwords are never stored here (the design keeps secrets in the OS
//! keychain, which is not built yet). A damaged file is moved aside instead of
//! crashing or being overwritten silently, and a file written by a newer
//! version is read but never saved over.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use rfb_security::tls::{Fingerprint, format_fingerprint, parse_fingerprint};
use serde::{Deserialize, Serialize};

/// The file format version this build writes.
pub const FORMAT_VERSION: u32 = 1;

/// Saved connections kept; the least recently used ones are dropped.
pub const MAX_SAVED_CONNECTIONS: usize = 50;

/// Appearance options (design doc, section 16). Stored as plain strings so
/// that an unknown value from a newer version degrades to the default instead
/// of making the whole file unreadable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// `glass`, `slate`, `light` or `system`.
    pub theme: String,
    /// `default`, `ember`, `cyan`, `violet`, `green` or `amber`.
    pub accent: String,
    /// Interface size in percent: 90, 100 or 115.
    pub interface_size: u32,
    pub reduce_transparency: bool,
    /// Show the bar with protocol, security and size under the picture.
    pub show_footer: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "glass".into(),
            accent: "default".into(),
            interface_size: 100,
            reduce_transparency: false,
            show_footer: true,
        }
    }
}

/// Input options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    /// Keyboard capture: `auto` (full screen only), `always` or `never`.
    pub capture: String,
    /// Resize the remote desktop to the window when the server allows it.
    pub resize_remote: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            capture: "auto".into(),
            resize_remote: false,
        }
    }
}

/// Video options.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    /// Ask servers for H.264 (needs Cisco's `OpenH264` library, which is
    /// downloaded on request and not part of Farpane).
    pub h264: bool,
}

/// One address book entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedConnection {
    pub name: String,
    pub addr: String,
    /// Seconds since the Unix epoch of the last successful connection.
    pub last_connected: u64,
}

/// A TLS certificate the user chose to trust for one server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHost {
    /// The address as typed (`host:port`).
    pub host: String,
    /// SHA-256 fingerprint of the certificate, `AB:CD:...`.
    pub sha256: String,
}

/// Everything the viewer remembers between runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub appearance: Appearance,
    pub input: Input,
    pub video: Video,
    #[serde(rename = "connection")]
    pub connections: Vec<SavedConnection>,
    /// Pinned certificates (trust on first use).
    #[serde(rename = "known_host")]
    pub known_hosts: Vec<KnownHost>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            appearance: Appearance::default(),
            input: Input::default(),
            video: Video::default(),
            connections: Vec::new(),
            known_hosts: Vec::new(),
        }
    }
}

/// The key two addresses are compared by: trimmed and case-insensitive.
fn key(addr: &str) -> String {
    addr.trim().to_lowercase()
}

impl Settings {
    /// Record a successful connection: add or update the entry for `addr`.
    pub fn remember(&mut self, name: &str, addr: &str, now: u64) {
        let addr = addr.trim();
        if addr.is_empty() {
            return;
        }
        let name = if name.trim().is_empty() {
            addr
        } else {
            name.trim()
        };
        match self
            .connections
            .iter_mut()
            .find(|c| key(&c.addr) == key(addr))
        {
            Some(entry) => {
                name.clone_into(&mut entry.name);
                entry.last_connected = now;
            }
            None => self.connections.push(SavedConnection {
                name: name.to_owned(),
                addr: addr.to_owned(),
                last_connected: now,
            }),
        }
        // Most recent first; keep the newest.
        self.connections
            .sort_by_key(|c| std::cmp::Reverse(c.last_connected));
        self.connections.truncate(MAX_SAVED_CONNECTIONS);
    }

    /// The fingerprints trusted for `addr` (unreadable entries are skipped).
    #[must_use]
    pub fn pins_for(&self, addr: &str) -> Vec<Fingerprint> {
        self.known_hosts
            .iter()
            .filter(|k| key(&k.host) == key(addr))
            .filter_map(|k| parse_fingerprint(&k.sha256))
            .collect()
    }

    /// Trust exactly `fingerprint` for `addr`, replacing earlier pins (a
    /// server has one certificate at a time; accepting a changed one retires
    /// the old pin).
    pub fn pin(&mut self, addr: &str, fingerprint: &Fingerprint) {
        let addr = addr.trim();
        self.known_hosts.retain(|k| key(&k.host) != key(addr));
        self.known_hosts.push(KnownHost {
            host: addr.to_owned(),
            sha256: format_fingerprint(fingerprint),
        });
    }

    /// Remove the entry for `addr`; returns whether there was one.
    pub fn forget(&mut self, addr: &str) -> bool {
        let before = self.connections.len();
        self.connections.retain(|c| key(&c.addr) != key(addr));
        self.connections.len() != before
    }

    /// Entries, most recently used first.
    #[must_use]
    pub fn recent(&self) -> Vec<&SavedConnection> {
        let mut all: Vec<&SavedConnection> = self.connections.iter().collect();
        all.sort_by_key(|c| std::cmp::Reverse(c.last_connected));
        all
    }
}

/// Why saving did not happen.
#[derive(Debug)]
pub enum SaveError {
    /// There is no usable configuration directory (tests, odd environments).
    NoPath,
    /// The file was written by a newer version; saving would lose its data.
    NewerFormat,
    Io(io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPath => write!(f, "no configuration directory"),
            Self::NewerFormat => write!(f, "settings file is from a newer version"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SaveError {}

/// The settings plus where they live.
#[derive(Debug)]
pub struct Store {
    path: Option<PathBuf>,
    read_only: bool,
    pub settings: Settings,
}

impl Store {
    /// The folder the settings file is in, if there is one.
    #[must_use]
    pub fn dir(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
    }

    /// Load from `path`. A missing file gives defaults; a damaged one is
    /// renamed to `<name>.broken` and replaced by defaults. The second value
    /// is a message for the user when something went wrong.
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> (Self, Option<String>) {
        let mut store = Self {
            path,
            read_only: false,
            settings: Settings::default(),
        };
        let Some(path) = store.path.clone() else {
            return (store, None);
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (store, None),
            Err(e) => {
                store.read_only = true;
                return (store, Some(format!("Could not read settings: {e}")));
            }
        };
        match toml::from_str::<Settings>(&text) {
            Ok(settings) if settings.version > FORMAT_VERSION => {
                store.read_only = true;
                store.settings = settings;
                (
                    store,
                    Some(
                        "Settings were saved by a newer Farpane; changes will not be saved".into(),
                    ),
                )
            }
            Ok(settings) => {
                store.settings = settings;
                (store, None)
            }
            Err(e) => {
                let broken = path.with_extension("toml.broken");
                let note = match fs::rename(&path, &broken) {
                    Ok(()) => format!("Settings were damaged and moved to {}", broken.display()),
                    Err(_) => format!("Settings were damaged ({})", e.message()),
                };
                (store, Some(note))
            }
        }
    }

    /// Write the settings: to a temporary file, then renamed over the real
    /// one, so a crash never leaves a half-written file.
    ///
    /// # Errors
    /// [`SaveError`].
    pub fn save(&self) -> Result<(), SaveError> {
        let path = self.path.as_ref().ok_or(SaveError::NoPath)?;
        if self.read_only {
            return Err(SaveError::NewerFormat);
        }
        let text = toml::to_string_pretty(&self.settings)
            .map_err(|e| SaveError::Io(io::Error::other(e)))?;
        write_atomically(path, text.as_bytes()).map_err(SaveError::Io)
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    {
        // `create_new` refuses a leftover file or a planted symlink, which
        // would keep its own (possibly wider) permissions.
        let _ = fs::remove_file(&tmp);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Where the viewer's settings file lives: `farpane/viewer.toml` in the
/// platform configuration directory.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("farpane").join("viewer.toml"))
}

/// "5 min ago" style text for the address book.
#[must_use]
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => plural(secs / 3600, "hour"),
        86_400..=2_591_999 => plural(secs / 86_400, "day"),
        _ => plural(secs / 2_592_000, "month"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_in(dir: &tempfile::TempDir) -> (Store, PathBuf) {
        let path = dir.path().join("farpane").join("viewer.toml");
        (Store::load(Some(path.clone())).0, path)
    }

    #[test]
    fn missing_file_gives_defaults_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (store, warning) = Store::load(Some(dir.path().join("nope.toml")));
        assert_eq!(store.settings, Settings::default());
        assert_eq!(warning, None);
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, path) = store_in(&dir);
        store.settings.appearance.theme = "slate".into();
        store.settings.appearance.interface_size = 115;
        store.settings.remember("Office PC", "10.0.0.5:5900", 1_000);
        store
            .settings
            .remember("Lab", "lab.example.org:5901", 2_000);
        store.save().unwrap();
        let (loaded, warning) = Store::load(Some(path.clone()));
        assert_eq!(warning, None);
        assert_eq!(loaded.settings, store.settings);
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "no temporary file left"
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[[connection]]") && text.contains("Office PC"),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (store, path) = store_in(&dir);
        store.save().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn unknown_keys_and_values_do_not_break_loading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewer.toml");
        fs::write(
            &path,
            "version = 1\nfuture_option = true\n[appearance]\ntheme = \"neon\"\nnew_thing = 3\n",
        )
        .unwrap();
        let (store, warning) = Store::load(Some(path));
        assert_eq!(warning, None);
        assert_eq!(
            store.settings.appearance.theme, "neon",
            "kept; the app falls back when it maps it"
        );
        assert_eq!(
            store.settings.appearance.interface_size, 100,
            "missing keys take defaults"
        );
    }

    #[test]
    fn a_damaged_file_is_moved_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewer.toml");
        fs::write(&path, "this is = not [valid toml").unwrap();
        let (store, warning) = Store::load(Some(path.clone()));
        assert_eq!(store.settings, Settings::default());
        assert!(warning.is_some_and(|w| w.contains("damaged")));
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(path.with_extension("toml.broken")).unwrap(),
            "this is = not [valid toml"
        );
        // Saving afterwards works and does not touch the backup.
        store.save().unwrap();
        assert!(path.exists() && path.with_extension("toml.broken").exists());
    }

    #[test]
    fn a_newer_format_is_read_but_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewer.toml");
        let original = "version = 99\nfuture_data = \"keep me\"\n";
        fs::write(&path, original).unwrap();
        let (store, warning) = Store::load(Some(path.clone()));
        assert!(warning.is_some_and(|w| w.contains("newer")));
        assert!(matches!(store.save(), Err(SaveError::NewerFormat)));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn remember_updates_instead_of_duplicating_and_orders_by_recency() {
        let mut s = Settings::default();
        s.remember("A", "host-a:5900", 100);
        s.remember("B", "host-b:5900", 200);
        s.remember("A again", " HOST-A:5900 ", 300);
        let names: Vec<_> = s
            .recent()
            .iter()
            .map(|c| (c.name.as_str(), c.last_connected))
            .collect();
        assert_eq!(names, [("A again", 300), ("B", 200)]);
        assert_eq!(s.connections.len(), 2);
        // An empty server name falls back to the address; empty addresses are ignored.
        s.remember("  ", "bare:1", 400);
        assert_eq!(s.recent()[0].name, "bare:1");
        s.remember("x", "   ", 500);
        assert_eq!(s.connections.len(), 3);
    }

    #[test]
    fn the_address_book_is_bounded_and_keeps_the_newest() {
        let mut s = Settings::default();
        for i in 0..(MAX_SAVED_CONNECTIONS as u64 + 10) {
            s.remember("n", &format!("h{i}:5900"), i);
        }
        assert_eq!(s.connections.len(), MAX_SAVED_CONNECTIONS);
        assert!(s.connections.iter().all(|c| c.last_connected >= 10));
    }

    #[test]
    fn forget_removes_one_entry() {
        let mut s = Settings::default();
        s.remember("A", "a:1", 1);
        s.remember("B", "b:1", 2);
        assert!(s.forget("A:1"));
        assert!(!s.forget("a:1"));
        assert_eq!(s.connections.len(), 1);
    }

    #[test]
    fn pins_are_per_server_replaced_not_accumulated_and_survive_the_file() {
        let mut s = Settings::default();
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert_eq!(s.pins_for("srv:5900").len(), 0);
        s.pin("srv:5900", &a);
        s.pin("other:5900", &b);
        assert_eq!(
            s.pins_for("SRV:5900"),
            [a],
            "names compare case-insensitively"
        );
        // Accepting a changed certificate retires the old pin.
        s.pin(" srv:5900 ", &b);
        assert_eq!(s.pins_for("srv:5900"), [b]);
        assert_eq!(s.known_hosts.len(), 2);

        let dir = tempfile::tempdir().unwrap();
        let (mut store, path) = store_in(&dir);
        store.settings = s.clone();
        store.save().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[[known_host]]") && text.contains("02:02:02"),
            "{text}"
        );
        assert_eq!(Store::load(Some(path)).0.settings.pins_for("srv:5900"), [b]);
    }

    #[test]
    fn a_damaged_pin_is_ignored_not_trusted() {
        let mut s = Settings::default();
        s.known_hosts.push(KnownHost {
            host: "srv:5900".into(),
            sha256: "not a fingerprint".into(),
        });
        assert_eq!(
            s.pins_for("srv:5900").len(),
            0,
            "an unreadable pin trusts nothing"
        );
    }

    #[test]
    fn without_a_path_saving_says_so() {
        let (store, warning) = Store::load(None);
        assert_eq!(warning, None);
        assert!(matches!(store.save(), Err(SaveError::NoPath)));
    }

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(ago(1000, 1000), "just now");
        assert_eq!(
            ago(1000, 2000),
            "just now",
            "clock skew never goes negative"
        );
        assert_eq!(ago(100, 40), "1 min ago");
        assert_eq!(ago(7200, 0), "2 hours ago");
        assert_eq!(ago(3600, 0), "1 hour ago");
        assert_eq!(ago(86_400 * 3, 0), "3 days ago");
        assert_eq!(ago(86_400 * 61, 0), "2 months ago");
    }
}
