//! What the dashboard reads from and does to the running server, behind a
//! trait so the window can be tested without one.

use std::{
    collections::VecDeque,
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use rfb_server::{ClientInfo, Preview, Server};

/// How serious a log line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
}

/// One line of the event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub at: SystemTime,
    pub level: Level,
    pub text: String,
}

/// The most lines kept.
const LOG_CAPACITY: usize = 1000;

/// The event log: connects, sign-in failures, lockouts and errors. Cheap to
/// clone; clones share the lines.
#[derive(Debug, Clone, Default)]
pub struct EventLog(Arc<Mutex<VecDeque<LogEntry>>>);

impl EventLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a line, dropping the oldest past the capacity.
    pub fn push(&self, level: Level, text: impl Into<String>) {
        let mut lines = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lines.len() >= LOG_CAPACITY {
            lines.pop_front();
        }
        lines.push_back(LogEntry {
            at: SystemTime::now(),
            level,
            text: text.into(),
        });
    }

    /// Everything kept, oldest first.
    #[must_use]
    pub fn lines(&self) -> Vec<LogEntry> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// The log as a JSON array of `{ "time": seconds since 1970, "level":
    /// "info" | "warning", "message": text }`, for export.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::from("[");
        for (i, l) in self.lines().iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let secs =
                l.at.duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
            let level = match l.level {
                Level::Info => "info",
                Level::Warning => "warning",
            };
            // Writing to a String cannot fail.
            let _ = write!(
                out,
                "\n  {{\"time\": {secs}, \"level\": \"{level}\", \"message\": \"{}\"}}",
                json_escape(&l.text)
            );
        }
        out.push_str("\n]\n");
        out
    }
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// Facts about the running server that do not change much.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Where it listens, such as `127.0.0.1:5900`.
    pub listen: String,
    /// What is shared, such as `X11 :0` or `test pattern`.
    pub source: String,
    /// How clients sign in and how the traffic is protected.
    pub access: String,
    /// Fingerprints a viewer asks its user to confirm, with a label each.
    pub fingerprints: Vec<(String, String)>,
    /// Why capturing stopped, if it did.
    pub failure: Option<String>,
    pub screen: (u16, u16),
    /// Where browsers connect, if anywhere.
    pub websocket: Option<String>,
    /// Sharing is paused.
    pub paused: bool,
    /// The ways to sign in: name, a note, and whether it is offered.
    pub offered: Vec<(String, String, bool)>,
}

/// The dashboard's view of the server.
pub trait Backend: Send + Sync {
    fn status(&self) -> Status;
    fn clients(&self) -> Vec<ClientInfo>;
    fn disconnect(&self, id: u64) -> bool;
    fn set_view_only(&self, id: u64, view_only: bool) -> bool;
    fn preview(&self, max_edge: u16) -> Preview;
    /// Pause or resume sharing.
    fn set_paused(&self, _paused: bool) {}
    /// Clients waiting for the operator's approval.
    fn pending(&self) -> Vec<rfb_server::PendingRequest> {
        Vec::new()
    }
    /// Answer a waiting client.
    fn decide(&self, _id: u64, _decision: rfb_server::Decision) -> bool {
        false
    }
    /// Put saved settings to work in the running server. `Err` says why that
    /// could not be done; they then apply at the next start.
    ///
    /// # Errors
    /// A message for the user.
    fn apply_settings(
        &self,
        _settings: &crate::settings::ServerSettings,
    ) -> Result<Applied, String> {
        Ok(Applied::Restart)
    }
}

/// What [`Backend::apply_settings`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The running server now follows the settings.
    Live,
    /// This backend cannot change them while running.
    Restart,
}

/// What the dashboard shows about how the server is set up, and what
/// changes when settings are applied live.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveInfo {
    /// Where it listens, such as `127.0.0.1:5900`.
    pub listen: String,
    /// How clients sign in and how the traffic is protected.
    pub access: String,
    /// Where WebSocket clients (browsers) connect, if anywhere.
    pub websocket: Option<String>,
    /// Fingerprints a viewer asks its user to confirm, with a label each.
    pub fingerprints: Vec<(String, String)>,
    /// The ways to sign in: name, a note, and whether it is offered.
    pub offered: Vec<(String, String, bool)>,
}

/// Puts settings to work in a running server and returns the new [`LiveInfo`].
pub type Applier =
    Arc<dyn Fn(&crate::settings::ServerSettings) -> Result<LiveInfo, String> + Send + Sync>;

/// The real thing: a running [`Server`].
pub struct LiveBackend {
    pub server: Arc<Server>,
    pub source: String,
    pub info: std::sync::Mutex<LiveInfo>,
    pub applier: Option<Applier>,
}

impl std::fmt::Debug for LiveBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveBackend")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl Backend for LiveBackend {
    fn status(&self) -> Status {
        let info = self
            .info
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Status {
            listen: info.listen,
            source: self.source.clone(),
            access: info.access,
            fingerprints: info.fingerprints,
            failure: self.server.capture_failure(),
            screen: self.server.size(),
            websocket: info.websocket,
            paused: self.server.is_paused(),
            offered: info.offered,
        }
    }

    fn clients(&self) -> Vec<ClientInfo> {
        self.server.clients()
    }

    fn disconnect(&self, id: u64) -> bool {
        self.server.disconnect(id)
    }

    fn set_view_only(&self, id: u64, view_only: bool) -> bool {
        self.server.set_view_only(id, view_only)
    }

    fn preview(&self, max_edge: u16) -> Preview {
        self.server.preview(max_edge)
    }

    fn set_paused(&self, paused: bool) {
        self.server.set_paused(paused);
    }

    fn apply_settings(
        &self,
        settings: &crate::settings::ServerSettings,
    ) -> Result<Applied, String> {
        let Some(apply) = &self.applier else {
            return Ok(Applied::Restart);
        };
        let info = apply(settings)?;
        *self
            .info
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = info;
        Ok(Applied::Live)
    }

    fn pending(&self) -> Vec<rfb_server::PendingRequest> {
        self.server.pending()
    }

    fn decide(&self, id: u64, decision: rfb_server::Decision) -> bool {
        self.server.decide(id, decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_the_newest_lines_and_exports_valid_json() {
        let log = EventLog::new();
        for i in 0..LOG_CAPACITY + 5 {
            log.push(Level::Info, format!("line {i}"));
        }
        let lines = log.lines();
        assert_eq!(lines.len(), LOG_CAPACITY);
        assert_eq!(lines[0].text, "line 5");
        log.clear();
        log.push(
            Level::Warning,
            "quote \" backslash \\ newline \n tab \t bell \u{7} é",
        );
        let json = log.to_json();
        assert!(json.contains("\"level\": \"warning\""), "{json}");
        assert!(
            json.contains(r#"quote \" backslash \\ newline \n tab \t bell \u0007 é"#),
            "{json}"
        );
        // The clones share the lines.
        let other = log.clone();
        other.push(Level::Info, "second");
        assert_eq!(log.lines().len(), 2);
    }
}
