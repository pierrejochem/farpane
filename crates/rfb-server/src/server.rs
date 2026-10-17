use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rfb_codec::{Cursor, Framebuffer};
use rfb_proto::types::Rect;
use rfb_tokio::{Error, ServerOptions, ServerSession};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
    sync::broadcast,
};

use crate::{
    client,
    registry::{ClientHandle, ClientInfo, Preview},
    source::{Input, Source},
    throttle::Throttle,
};

/// What the capture thread tells the clients.
#[derive(Debug, Clone)]
pub(crate) struct Update {
    pub rects: Vec<Rect>,
    /// Parts of the picture that moved (scrolling), found by comparing with
    /// the previous one. Each is also inside `rects`.
    pub copies: Vec<crate::scroll::Copy>,
    /// The pointer has a new shape.
    pub cursor: Option<Arc<Cursor>>,
    /// New text on the server's clipboard.
    pub clipboard: Option<Arc<str>>,
    /// Capturing has stopped for good; clients should disconnect.
    pub ended: bool,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub fb: RwLock<Framebuffer>,
    pub updates: broadcast::Sender<Arc<Update>>,
    /// The pointer's current shape, for clients that join later.
    pub cursor: RwLock<Option<Arc<Cursor>>>,
    /// Connected clients, for the dashboard.
    pub registry: Mutex<Vec<Arc<ClientHandle>>>,
    next_id: AtomicU64,
    pub input: Arc<dyn Input>,
    /// H.264 encoders, when a codec was supplied.
    pub video: RwLock<Option<Arc<dyn rfb_codec::video::VideoCodec>>>,
    stop: AtomicBool,
    /// Sharing is paused: clients keep their last picture and their input
    /// is ignored.
    pub paused: AtomicBool,
    /// Why capturing stopped, if it did.
    failure: Mutex<Option<String>>,
    clients: AtomicUsize,
}

/// Server behaviour that is not about one connection.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Capture frequency.
    pub fps: u32,
    /// Ignore keyboard, pointer and clipboard input from clients.
    pub view_only: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 30,
            view_only: false,
        }
    }
}

/// A running server: the capture thread and the shared picture. Dropping it
/// stops capturing; running connections end.
pub struct Server {
    shared: Arc<Shared>,
    template: RwLock<ServerOptions>,
    view_only: AtomicBool,
    policy: RwLock<Option<Policy>>,
    events: Option<Sink>,
    throttle: Mutex<Throttle>,
    /// Connections being served or still in their handshake.
    connections: AtomicUsize,
    max_connections: usize,
    max_per_address: usize,
    /// Hold every signed-in client until the operator allows it.
    require_approval: AtomicBool,
    approval_timeout: Duration,
    waiting: Mutex<Vec<Waiting>>,
    next_waiting: AtomicU64,
    websocket: RwLock<WebSocketConfig>,
}

/// How the WebSocket listener treats connections; changeable while running.
#[derive(Clone, Default)]
struct WebSocketConfig {
    tls: Option<Arc<rustls::ServerConfig>>,
    origins: rfb_tokio::ws::OriginPolicy,
}

/// A client that signed in and waits for the operator.
struct Waiting {
    id: u64,
    peer: Option<SocketAddr>,
    user: Option<String>,
    encryption: &'static str,
    since: Instant,
    decide: Option<tokio::sync::oneshot::Sender<Decision>>,
}

/// What the operator decided about a waiting client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Let it in with the rights its account has.
    Allow,
    /// Let it in, but ignore its keyboard, pointer and clipboard.
    ViewOnly,
    /// Close the connection.
    Deny,
}

/// A client waiting for approval, as the dashboard lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub id: u64,
    pub peer: Option<SocketAddr>,
    pub user: Option<String>,
    /// `"TLS"`, `"RSA-AES"` or `"none"`.
    pub encryption: &'static str,
    pub waiting_for: Duration,
}

/// How long a client waits for approval before it is turned away.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Removes a waiting client when its task ends, however it ends.
struct WaitingGuard<'a>(&'a Mutex<Vec<Waiting>>, u64);

impl Drop for WaitingGuard<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|w| w.id != self.1);
    }
}

/// Connections allowed at once unless [`Server::set_max_connections`] says otherwise.
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

/// Connections one address may hold at once unless
/// [`Server::set_max_per_address`] says otherwise.
pub const DEFAULT_MAX_PER_ADDRESS: usize = 32;

/// Counts a connection against its address for as long as it lives.
struct AddressSlot<'a>(&'a Mutex<Throttle>, std::net::IpAddr);

impl Drop for AddressSlot<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .leave(self.1);
    }
}

/// Counts a connection for as long as it lives.
struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("view_only", &self.view_only.load(Ordering::Relaxed))
            .field("clients", &self.client_count())
            .finish_non_exhaustive()
    }
}

type Sink = Arc<dyn Fn(&Event) + Send + Sync>;

/// What a signed-in user may do, by account name.
pub type Policy = Arc<dyn Fn(Option<&str>) -> Permission + Send + Sync>;

/// What a signed-in user may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Look and control.
    Full,
    /// Look only: keyboard, pointer and clipboard input is dropped.
    ViewOnly,
}

/// Something worth logging, from [`Server::set_events`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A client signed in. `user` is the account name for password logins
    /// with a user name, `None` for the shared-password and no-password types.
    Connected {
        peer: Option<SocketAddr>,
        user: Option<String>,
        permission: Permission,
    },
    /// A client's session ended; `error` says why if it was not a plain hang-up.
    Closed {
        peer: Option<SocketAddr>,
        user: Option<String>,
        error: Option<String>,
    },
    /// Wrong or missing credentials.
    AuthFailed { peer: Option<SocketAddr> },
    /// A client was refused entry: denied by the operator or not answered in time.
    Denied {
        peer: Option<SocketAddr>,
        user: Option<String>,
        reason: String,
    },
    /// The handshake failed for another reason (timeout, bad protocol, TLS).
    HandshakeFailed {
        peer: Option<SocketAddr>,
        error: String,
    },
}

impl Server {
    /// Take a first frame from `source` and start capturing.
    ///
    /// `template` gives the authentication, TLS and name; the size is taken
    /// from the screen for every connection.
    ///
    /// # Errors
    /// The first capture failing, or a screen too large for the protocol.
    pub fn new(
        mut source: impl Source,
        input: impl Input,
        template: ServerOptions,
        settings: Settings,
    ) -> io::Result<Self> {
        let (w, h) = source.size();
        let mut fb = Framebuffer::new(w, h).map_err(io::Error::other)?;
        let first = source.capture(&mut fb)?;
        let (updates, _) = broadcast::channel(64);
        let shared = Arc::new(Shared {
            fb: RwLock::new(fb),
            updates,
            cursor: RwLock::new(first.cursor.map(Arc::new)),
            registry: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            input: Arc::new(input),
            video: RwLock::new(None),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            failure: Mutex::new(None),
            clients: AtomicUsize::new(0),
        });
        let weak = Arc::clone(&shared);
        let interval = Duration::from_secs(1) / settings.fps.clamp(1, 240);
        thread::Builder::new()
            .name("farpane-capture".into())
            .spawn(move || capture_loop(source, &weak, interval))?;
        Ok(Self {
            shared,
            template: RwLock::new(template),
            view_only: AtomicBool::new(settings.view_only),
            policy: RwLock::new(None),
            events: None,
            throttle: Mutex::new(Throttle::new()),
            connections: AtomicUsize::new(0),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_per_address: DEFAULT_MAX_PER_ADDRESS,
            require_approval: AtomicBool::new(false),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            waiting: Mutex::new(Vec::new()),
            next_waiting: AtomicU64::new(1),
            websocket: RwLock::new(WebSocketConfig::default()),
        })
    }

    /// Decide per user what a signed-in client may do. Without a policy every
    /// client gets [`Permission::Full`] (unless the server is view-only).
    pub fn set_policy(&self, policy: impl Fn(Option<&str>) -> Permission + Send + Sync + 'static) {
        *self
            .policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(policy));
    }

    /// Replace how clients sign in (and what each may do) while running. New
    /// connections use the new options; those already connected are left
    /// alone. `policy` `None` means every client gets full rights.
    pub fn replace_options(&self, template: ServerOptions, policy: Option<Policy>) {
        *self
            .template
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = template;
        *self
            .policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
    }

    /// Ignore keyboard, pointer and clipboard input from every client that
    /// connects from now on (and, with `true`, from those connected already).
    pub fn set_view_only_for_all(&self, on: bool) {
        self.view_only.store(on, Ordering::Relaxed);
        if on {
            for client in self
                .shared
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
            {
                client.view_only.store(true, Ordering::Relaxed);
            }
        }
    }

    /// The most connections served at once, handshakes included: further ones
    /// are dropped at once. Bounds the memory and the password-hashing work a
    /// flood of connections can cause.
    pub fn set_max_connections(&mut self, max: usize) {
        self.max_connections = max.max(1);
    }

    /// The most connections one address may hold at once (an IPv6 /64 counts
    /// as one address), so a single machine cannot fill every slot.
    pub fn set_max_per_address(&mut self, max: usize) {
        self.max_per_address = max.max(1);
    }

    /// Hold every client that signed in until the operator allows it (see
    /// [`Server::pending`] and [`Server::decide`]). The client sees an open
    /// connection but gets no pictures meanwhile; with no answer in the
    /// timeout it is turned away.
    pub fn set_require_approval(&self, on: bool) {
        self.require_approval.store(on, Ordering::Relaxed);
    }

    /// How long a client waits for approval.
    pub fn set_approval_timeout(&mut self, timeout: Duration) {
        self.approval_timeout = timeout;
    }

    /// Clients waiting for approval, oldest first.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingRequest> {
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|w| PendingRequest {
                id: w.id,
                peer: w.peer,
                user: w.user.clone(),
                encryption: w.encryption,
                waiting_for: w.since.elapsed(),
            })
            .collect()
    }

    /// Answer a waiting client. `false` if it is no longer waiting.
    pub fn decide(&self, id: u64, decision: Decision) -> bool {
        let sender = self
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
            .find(|w| w.id == id)
            .and_then(|w| w.decide.take());
        sender.is_some_and(|tx| tx.send(decision).is_ok())
    }

    /// Receive [`Event`]s: sign-ins, hang-ups, failures.
    pub fn set_events(&mut self, sink: impl Fn(&Event) + Send + Sync + 'static) {
        self.events = Some(Arc::new(sink));
    }

    fn emit(&self, event: &Event) {
        if let Some(sink) = &self.events {
            sink(event);
        }
    }

    /// The picture size now.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let fb = self
            .shared
            .fb
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (fb.width(), fb.height())
    }

    /// Offer H.264 (the Open H.264 encoding) to clients that ask for it,
    /// encoding with `codec`, or stop offering it with `None`. Clients that
    /// are connected keep what they use; new updates for clients that list
    /// the encoding switch over. Off by default.
    pub fn set_video(&self, codec: Option<Arc<dyn rfb_codec::video::VideoCodec>>) {
        *self
            .shared
            .video
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = codec;
    }

    /// Pause or resume sharing. While paused clients stay connected but get
    /// no new pictures, and their keyboard, pointer and clipboard input is
    /// ignored (held keys are let go). Changes made meanwhile are sent on
    /// resume.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    /// Whether sharing is paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// Why capturing stopped, if it did. Clients are disconnected then.
    #[must_use]
    pub fn capture_failure(&self) -> Option<String> {
        self.shared
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The clients connected now, oldest first.
    #[must_use]
    pub fn clients(&self) -> Vec<ClientInfo> {
        self.shared
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|h| h.info())
            .collect()
    }

    fn handle(&self, id: u64) -> Option<Arc<ClientHandle>> {
        self.shared
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|h| h.id == id)
            .cloned()
    }

    /// End a client's connection. Returns whether there was such a client.
    pub fn disconnect(&self, id: u64) -> bool {
        self.handle(id).is_some_and(|h| {
            h.kick.notify_one();
            true
        })
    }

    /// Let a client look only (or control again) from now on. Returns
    /// whether there was such a client.
    pub fn set_view_only(&self, id: u64, view_only: bool) -> bool {
        self.handle(id).is_some_and(|h| {
            h.view_only.store(view_only, Ordering::Relaxed);
            true
        })
    }

    /// A copy of the screen at most `max_edge` pixels on its longer side
    /// (nearest-neighbour), for a live preview.
    #[must_use]
    pub fn preview(&self, max_edge: u16) -> Preview {
        let fb = self
            .shared
            .fb
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (w, h) = (usize::from(fb.width()), usize::from(fb.height()));
        let longest = w.max(h).max(1);
        let edge = usize::from(max_edge.max(1));
        let (pw, ph) = if longest <= edge {
            (w, h)
        } else {
            ((w * edge / longest).max(1), (h * edge / longest).max(1))
        };
        let pixels = fb.pixels();
        let stride = fb.stride();
        let mut rgba = Vec::with_capacity(pw * ph * 4);
        for y in 0..ph {
            let sy = y * h / ph;
            for x in 0..pw {
                let at = sy * stride + (x * w / pw) * 4;
                rgba.extend_from_slice(&[pixels[at + 2], pixels[at + 1], pixels[at], 0xFF]);
            }
        }
        Preview {
            // At most `max_edge`, which is a u16.
            width: u16::try_from(pw).unwrap_or(u16::MAX),
            height: u16::try_from(ph).unwrap_or(u16::MAX),
            rgba,
        }
    }

    /// Clients currently past the handshake.
    #[must_use]
    pub fn client_count(&self) -> usize {
        self.shared.clients.load(Ordering::Relaxed)
    }

    /// Serve one connection until it ends. Same as [`Server::serve`] without
    /// a peer address (no per-address delays, events carry no address).
    ///
    /// # Errors
    /// Handshake and protocol errors; [`Error::Closed`] when the client hangs up.
    pub async fn serve_connection<S>(&self, stream: S) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.serve(stream, None).await
    }

    /// Serve one connection from `peer` until it ends. A peer that has
    /// failed to sign in recently waits before its handshake starts.
    ///
    /// # Errors
    /// Handshake and protocol errors; [`Error::Closed`] when the client hangs up.
    pub async fn serve<S>(&self, stream: S, peer: Option<SocketAddr>) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.serve_over(stream, peer, false).await
    }

    /// Like [`Server::serve`] for a stream that is already encrypted, such as
    /// a TLS WebSocket (`wss`): user names and passwords may then be sent as
    /// `VeNCrypt` `Plain`, and the client is listed as protected by TLS.
    ///
    /// # Errors
    /// As [`Server::serve`].
    pub async fn serve_secure<S>(&self, stream: S, peer: Option<SocketAddr>) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.serve_over(stream, peer, true).await
    }

    #[allow(clippy::too_many_lines)] // handshake, registration, session, cleanup
    async fn serve_over<S>(
        &self,
        stream: S,
        peer: Option<SocketAddr>,
        secure: bool,
    ) -> Result<(), Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let in_use = self.connections.fetch_add(1, Ordering::Relaxed) + 1;
        let _slot = Slot(&self.connections);
        if in_use > self.max_connections {
            let error = Error::Config("too many connections");
            self.emit(&Event::HandshakeFailed {
                peer,
                error: error.to_string(),
            });
            return Err(error);
        }
        let _address = match peer {
            Some(p) => {
                let admitted = self
                    .throttle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .enter(p.ip(), self.max_per_address);
                if !admitted {
                    let error = Error::Config("too many connections from this address");
                    self.emit(&Event::HandshakeFailed {
                        peer,
                        error: error.to_string(),
                    });
                    return Err(error);
                }
                Some(AddressSlot(&self.throttle, p.ip()))
            }
            None => None,
        };
        if let Some(p) = peer {
            let wait = self
                .throttle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .delay(p.ip(), Instant::now());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
        let (w, h) = self.size();
        let mut opts = self
            .template
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        opts.width = w;
        opts.height = h;
        if secure {
            // The outer TLS replaces the one `VeNCrypt` would start.
            opts.tls = None;
            opts.secure_transport = true;
        }
        let session = match ServerSession::accept(stream, opts).await {
            Ok(s) => s,
            Err(e) => {
                if let Some(p) = peer {
                    if matches!(e, Error::AuthFailed) {
                        self.throttle
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .record_failure(p.ip(), Instant::now());
                    }
                }
                if matches!(e, Error::AuthFailed) {
                    self.emit(&Event::AuthFailed { peer });
                } else if !is_hang_up(&e) {
                    self.emit(&Event::HandshakeFailed {
                        peer,
                        error: e.to_string(),
                    });
                }
                return Err(e);
            }
        };
        if let Some(p) = peer {
            self.throttle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_success(p.ip());
        }
        let user = session
            .username()
            .map(|u| String::from_utf8_lossy(u).into_owned());
        let permission = if self.view_only.load(Ordering::Relaxed) {
            Permission::ViewOnly
        } else {
            self.policy
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map_or(Permission::Full, |f| f(user.as_deref()))
        };
        let encryption = if secure || session.is_encrypted() {
            "TLS"
        } else if session.is_ra2() {
            "RSA-AES"
        } else {
            "none"
        };
        let mut permission = permission;
        if self.require_approval.load(Ordering::Relaxed) {
            match self.await_approval(peer, user.clone(), encryption).await {
                Ok(Decision::Allow) => {}
                Ok(Decision::ViewOnly) => permission = Permission::ViewOnly,
                Ok(Decision::Deny) => {
                    self.emit(&Event::Denied {
                        peer,
                        user,
                        reason: "denied from the dashboard".to_owned(),
                    });
                    return Err(Error::Closed);
                }
                Err(reason) => {
                    self.emit(&Event::Denied { peer, user, reason });
                    return Err(Error::Closed);
                }
            }
        }
        self.emit(&Event::Connected {
            peer,
            user: user.clone(),
            permission,
        });
        self.shared.clients.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(ClientHandle {
            id: self.shared.next_id.fetch_add(1, Ordering::Relaxed),
            peer,
            user: user.clone(),
            since: Instant::now(),
            encryption,
            view_only: AtomicBool::new(permission == Permission::ViewOnly),
            kick: tokio::sync::Notify::new(),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            updates: AtomicU64::new(0),
            encoding: AtomicI32::new(0),
            rtt_micros: AtomicU64::new(0),
        });
        self.shared
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::clone(&handle));
        let result = client::run(session, Arc::clone(&self.shared), Arc::clone(&handle)).await;
        self.shared
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|h| h.id != handle.id);
        self.shared.clients.fetch_sub(1, Ordering::Relaxed);
        self.emit(&Event::Closed {
            peer,
            user,
            error: match &result {
                Err(e) if !is_hang_up(e) => Some(e.to_string()),
                _ => None,
            },
        });
        result
    }

    /// Wait for the operator. `Err` says why the client is turned away.
    async fn await_approval(
        &self,
        peer: Option<SocketAddr>,
        user: Option<String>,
        encryption: &'static str,
    ) -> Result<Decision, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = self.next_waiting.fetch_add(1, Ordering::Relaxed);
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Waiting {
                id,
                peer,
                user,
                encryption,
                since: Instant::now(),
                decide: Some(tx),
            });
        let _guard = WaitingGuard(&self.waiting, id);
        match tokio::time::timeout(self.approval_timeout, rx).await {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(_)) => Err("the request was dropped".to_owned()),
            Err(_) => Err(format!(
                "nobody answered within {} s",
                self.approval_timeout.as_secs()
            )),
        }
    }

    /// How WebSocket connections are accepted by [`Server::listen_ws`]: with
    /// `tls` they are `wss` (TLS first, then clients may sign in with
    /// `VeNCrypt` `Plain` and `RA2ne`); without it they are `ws` and carry no
    /// more protection than the RFB handshake gives. `origins` decides which
    /// web pages may connect. Takes effect for the next connection.
    pub fn set_websocket(
        &self,
        tls: Option<Arc<rustls::ServerConfig>>,
        origins: rfb_tokio::ws::OriginPolicy,
    ) {
        *self
            .websocket
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = WebSocketConfig { tls, origins };
    }

    /// Accept WebSocket connections on `listener` for ever (noVNC and other
    /// browser clients), as set by [`Server::set_websocket`].
    ///
    /// # Errors
    /// The listener failing.
    pub async fn listen_ws(self: Arc<Self>, listener: TcpListener) -> io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            let WebSocketConfig { tls, origins } = self
                .websocket
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                let step = Duration::from_secs(30);
                let result = async {
                    if let Some(config) = tls {
                        let tls =
                            tokio::time::timeout(step, rfb_tokio::ws::accept_tls(stream, config))
                                .await
                                .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
                        let ws = tokio::time::timeout(step, rfb_tokio::ws::accept(tls, &origins))
                            .await
                            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
                        Ok::<_, io::Error>(Either::Secure(ws))
                    } else {
                        let ws =
                            tokio::time::timeout(step, rfb_tokio::ws::accept(stream, &origins))
                                .await
                                .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
                        Ok(Either::Open(ws))
                    }
                }
                .await;
                match result {
                    Ok(Either::Secure(ws)) => {
                        let _ = server.serve_secure(ws, Some(peer)).await;
                    }
                    Ok(Either::Open(ws)) => {
                        let _ = server.serve(ws, Some(peer)).await;
                    }
                    Err(e) => server.emit(&Event::HandshakeFailed {
                        peer: Some(peer),
                        error: format!("WebSocket: {e}"),
                    }),
                }
            });
        }
    }

    /// Accept connections on `listener` for ever, one task each. Use
    /// [`Server::set_events`] to hear about them.
    ///
    /// # Errors
    /// The listener failing.
    pub async fn listen(self: Arc<Self>, listener: TcpListener) -> io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                // Everything worth reporting went to the event sink.
                let _ = server.serve(stream, Some(peer)).await;
            });
        }
    }
}

/// Which kind of WebSocket connection was set up.
enum Either<A, B> {
    Secure(A),
    Open(B),
}

/// A client going away is normal, not an error to report.
fn is_hang_up(e: &Error) -> bool {
    matches!(e, Error::Closed)
        || matches!(e, Error::Io(io) if matches!(
            io.kind(),
            io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::UnexpectedEof
        ))
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// The scrolled part of what changed, if any: tried on every damaged
/// rectangle and on their bounding box (an application usually reports a
/// scroll as several rectangles).
fn find_scroll(old: &[u8], new: &[u8], stride: usize, damage: &[Rect]) -> Vec<crate::scroll::Copy> {
    let mut candidates: Vec<Rect> = damage.to_vec();
    if damage.len() > 1 {
        if let Some(all) = damage.iter().copied().reduce(crate::region::union) {
            candidates.push(all);
        }
    }
    candidates
        .into_iter()
        .find_map(|r| {
            crate::scroll::find_copy(old, new, stride, r)
                .or_else(|| crate::scroll::find_copy_sideways(old, new, stride, r))
        })
        .into_iter()
        .collect()
}

fn capture_loop(mut source: impl Source, shared: &Arc<Shared>, interval: Duration) {
    // The only strong holders besides this thread are the `Server` and its
    // connections; `stop` is set when the `Server` goes away.
    // The picture as it was after the previous capture, to spot scrolling.
    let mut previous: Vec<u8> = shared
        .fb
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pixels()
        .to_vec();
    while !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(interval);
        let mut copies = Vec::new();
        let result = {
            let mut fb = shared
                .fb
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let before = (fb.width(), fb.height());
            let result = source.capture(&mut fb);
            let after = (fb.width(), fb.height());
            // A resize damages the whole picture whatever the source said.
            result.map(|mut c| {
                if before == after && previous.len() == fb.pixels().len() {
                    copies = find_scroll(&previous, fb.pixels(), fb.stride(), &c.damage);
                }
                // Remember the new picture where it changed.
                if before != after || previous.len() != fb.pixels().len() {
                    previous = fb.pixels().to_vec();
                } else {
                    let stride = fb.stride();
                    for r in &c.damage {
                        for row in usize::from(r.y)..usize::from(r.y) + usize::from(r.height) {
                            let at = row * stride + usize::from(r.x) * 4;
                            let len = usize::from(r.width) * 4;
                            previous[at..at + len].copy_from_slice(&fb.pixels()[at..at + len]);
                        }
                    }
                }
                if before != after {
                    c.damage = vec![Rect {
                        x: 0,
                        y: 0,
                        width: after.0,
                        height: after.1,
                    }];
                }
                c
            })
        };
        match result {
            Ok(capture)
                if capture.damage.is_empty()
                    && capture.cursor.is_none()
                    && capture.clipboard.is_none() => {}
            Ok(capture) => {
                let cursor = capture.cursor.map(Arc::new);
                if let Some(c) = &cursor {
                    *shared
                        .cursor
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(c));
                }
                // No receivers just means nobody is connected.
                let _ = shared.updates.send(Arc::new(Update {
                    rects: capture.damage,
                    copies,
                    cursor,
                    clipboard: capture.clipboard.map(Arc::from),
                    ended: false,
                }));
            }
            Err(e) => {
                *shared
                    .failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e.to_string());
                let _ = shared.updates.send(Arc::new(Update {
                    rects: Vec::new(),
                    copies: Vec::new(),
                    cursor: None,
                    clipboard: None,
                    ended: true,
                }));
                return;
            }
        }
    }
}
