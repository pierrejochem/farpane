//! What the dashboard can see and do about connected clients.

use std::{
    net::SocketAddr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use tokio::sync::Notify;

/// Live state of one connected client, shared between its task and the
/// server's public methods.
#[derive(Debug)]
pub(crate) struct ClientHandle {
    pub id: u64,
    pub peer: Option<SocketAddr>,
    pub user: Option<String>,
    pub since: Instant,
    pub encryption: &'static str,
    /// Input from this client is dropped. Can change while it is connected.
    pub view_only: AtomicBool,
    /// Ask the task to end the connection.
    pub kick: Notify,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub updates: AtomicU64,
    /// The encoding of the last rectangle sent.
    pub encoding: AtomicI32,
    /// Round trip of the last fence, in microseconds (0 until one came back).
    pub rtt_micros: AtomicU64,
}

impl ClientHandle {
    pub(crate) fn info(&self) -> ClientInfo {
        ClientInfo {
            id: self.id,
            peer: self.peer,
            user: self.user.clone(),
            connected_for: self.since.elapsed(),
            encryption: self.encryption,
            view_only: self.view_only.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            updates: self.updates.load(Ordering::Relaxed),
            encoding: self.encoding.load(Ordering::Relaxed),
            rtt: match self.rtt_micros.load(Ordering::Relaxed) {
                0 => None,
                us => Some(Duration::from_micros(us)),
            },
        }
    }
}

/// A reading of one client, for a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub id: u64,
    pub peer: Option<SocketAddr>,
    /// The account name, for sign-ins by name.
    pub user: Option<String>,
    pub connected_for: Duration,
    /// `"TLS"`, `"RSA-AES"` or `"none"`.
    pub encryption: &'static str,
    pub view_only: bool,
    /// Bytes read from and written to the client so far.
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// `FramebufferUpdate` messages sent.
    pub updates: u64,
    /// Encoding number of the last rectangle sent (0 until one was).
    pub encoding: i32,
    /// Round trip to the client from the last fence (clients that support
    /// Fence only).
    pub rtt: Option<Duration>,
}

/// A small copy of the shared screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub width: u16,
    pub height: u16,
    /// `R, G, B, 0xFF` rows.
    pub rgba: Vec<u8>,
}
